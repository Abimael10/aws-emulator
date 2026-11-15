// SPDX-License-Identifier: CC0-1.0

//! This service's request-DESERIALIZATION layer for the values bound into the request
//! URI: how a path label or a query parameter that cannot be CONVERTED to the modelled
//! type of the member it is bound to is refused.
//!
//! # The rule
//!
//! Two refusals happen before a handler runs, and they are not the same error class:
//!
//! * a value bound into the URI — a `@httpLabel` path segment, a `@httpQuery` parameter —
//!   that cannot be converted to the modelled type of its member (`versions/latest` where
//!   the member targets a long, `MaxItems=lots` where it targets an integer) never becomes
//!   a value at all. It fails in the protocol's deserializer, ahead of the operation, so
//!   what answers is not an error shape of this model: the request never became a modelled
//!   input. It is answered as the protocol's own failure — HTTP 400 naming
//!   [`SERIALIZATION_EXCEPTION`], with the sentence under the CAPITALIZED
//!   [`MESSAGE_MEMBER`] the framework's envelope carries it in, stating the two facts a
//!   caller has to reconcile: the value it sent and the type that value could not become;
//! * a value that WAS converted and then broke a rule the model declares (an absent
//!   `@required` member, a `@length` / `@range` / `@pattern` / enum violation) is a
//!   constraint refusal, which this service states as the validation envelope over the
//!   clause the model's own constraint produced (see [`crate::service::validation`]).
//!
//! The framework renders BOTH as `400 ValidationException` with a lowercase `message`
//! (`vendor/smithy-server-runtime/src/pre_dispatch.rs`, where every `ProtocolError::Serde`
//! takes that code and the JSON envelope's member name is fixed), so a client branching on
//! the error type to decide whether to fix its request or fix a value takes the wrong
//! branch, and a client reading `Message` on a protocol error of this surface sees nothing
//! at all. The vendored tree is read-only from a service crate, so the separation is made
//! here, at the one seam this service has in front of dispatch — the wrapper every modelled
//! operation is already served behind ([`crate::service::StatedPerOperation`]) — exactly as
//! `services/service-com-amazonaws-apigateway-backplanecontrolservice/src/service/query_coercion.rs`
//! does for the same error class on its own surface.
//!
//! # Which members this layer answers for, and how it decides
//!
//! Nothing about the surface is written down here: the members are read off the operation's
//! own input schema — every member bound into the URI whose target is one of the scalar
//! types a URI value is CONVERTED into ([`converted_type_name`]). A member whose
//! conversion has no such stated form (a string, a timestamp, a blob, an enum, a list) is
//! left to the framework, which refuses it in its own envelope. The members are walked in
//! the input schema's own order, which is the order the framework's HTTP-binding reader
//! walks them in, so the member reported is the one that reader would have failed on first.
//!
//! The conversion test mirrors the framework's own single-value reader
//! (`vendor/smithy-schema-server/src/http_binding.rs`'s `StringValueDeserializer`: `"true"`
//! / `"false"` for a boolean, `str::parse` at the modelled width for a number) and calls
//! the same `str::parse`, while the values are read the way that reader reads them — a
//! label matched against the operation's own `@http` URI and percent-decoded as a path
//! component ([`service_common::percent_decode_path_label`]), a query parameter through the
//! framework's own `parse_form_urlencoded` — so the two layers cannot disagree about which
//! values convert. The unit test below pins that agreement against the vendored reader
//! itself.

use aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};
use aws_smithy_schema::{Schema, ShapeType};
use aws_smithy_types::body::SdkBody;

/// The error type a value the protocol could not convert is refused with.
///
/// It is the PROTOCOL's error, not one of this service's modelled shapes: the request never
/// became an input, so no operation ran to answer with an error of its own. What it must
/// NOT be is the constraint-refusal type, which is the type for a value that was read and
/// then broke a modelled constraint.
pub const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The body member the framework's error envelope carries its sentence in.
///
/// Capital `Message`, which is what distinguishes that envelope from a modelled error of
/// this service, whose member is the lowercase `message` its model declares.
pub const MESSAGE_MEMBER: &str = "Message";

/// The status a protocol-level refusal of the request carries: the caller sent something
/// that cannot be read, so it is a client error.
const UNCONVERTIBLE_STATUS: u16 = 400;

/// The media type every response of this restJson1 service carries.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The header restJson1 resolves an error type from.
const ERRORTYPE_HEADER: &str = "x-amzn-errortype";

/// The wire answer to a URI value this service cannot convert: `400` naming
/// [`SERIALIZATION_EXCEPTION`] in the header restJson1 reads an error type from, with the
/// sentence under the capital-[`MESSAGE_MEMBER`] body member the framework's envelope words
/// a protocol error with.
///
/// The body carries that member ALONE: it is the framework's envelope rather than an error
/// shape of this model, so it names neither a shape of this service nor the class of fault
/// this service's own shapes carry.
pub fn serialization_exception(message: &str) -> RtResponse {
    let body = serde_json::json!({ MESSAGE_MEMBER: message });
    let mut response = RtResponse::new(
        StatusCode::try_from(UNCONVERTIBLE_STATUS)
            .expect("the status is a constant of this module"),
        SdkBody::from(body.to_string()),
    );
    response
        .headers_mut()
        .insert("content-type", JSON_CONTENT_TYPE);
    response
        .headers_mut()
        .insert(ERRORTYPE_HEADER, SERIALIZATION_EXCEPTION);
    response
}

/// The sentence this layer refuses the request at `uri` with, for the operation whose
/// input `input_schema` describes, or `None` when every URI value that request carries
/// converts — the overwhelmingly common case.
///
/// The schema comes from the CALLER — the wrapper is generic over the operation it serves,
/// so it hands this its `O::INPUT_SCHEMA` — which is why this module needs no list of the
/// service's operations and cannot drift from one.
pub fn refusal_for(input_schema: &'static Schema, uri: &str) -> Option<String> {
    let labels = label_values(input_schema, ::service_common::request_path(uri));
    let query = match uri.split_once('?') {
        Some((_, query)) => ::aws_smithy_schema_server::parse_form_urlencoded(query.as_bytes()),
        None => Vec::new(),
    };
    for member in input_schema.members() {
        let Some(named) = converted_type_name(member.shape_type()) else {
            continue;
        };
        let Some(value) = bound_value(member, &labels, &query) else {
            continue;
        };
        if !converts(value, member.shape_type()) {
            return Some(unconvertible_message(value, named));
        }
    }
    None
}

/// The URI value bound to `member`, or `None` when the request binds it none — which
/// leaves the member absent rather than unconvertible.
///
/// A member is bound by exactly one binding, so the binding decides where the value is
/// read from. Only the FIRST value bound to a query name is read, as the framework's own
/// reader reads it for a non-list member, so a later duplicate is never converted and must
/// not be refused.
fn bound_value<'v>(
    member: &'static Schema,
    labels: &'v [(&'static str, String)],
    query: &'v [(String, String)],
) -> Option<&'v str> {
    if member.http_label().is_some() {
        let name = member.member_name()?;
        return labels
            .iter()
            .find(|(bound, _)| *bound == name)
            .map(|(_, value)| value.as_str());
    }
    let name = member.http_query()?.value();
    query
        .iter()
        .find(|(bound, _)| bound == name)
        .map(|(_, value)| value.as_str())
}

/// The value each `@httpLabel` member of `input_schema` is bound to by `path`, keyed by the
/// member's own name.
///
/// The pattern is the operation's own `@http` URI, read off the input schema the framework
/// reads it off too, and the walk is that framework's
/// (`vendor/smithy-schema-server/src/http_binding.rs`'s `match_path_labels`): the pattern's
/// literal segments have to match, a `{Label}` segment takes one path segment, and a
/// `{Label+}` segment takes every segment that is left. A path that does not match the
/// pattern binds only the labels matched before the mismatch, which is what that reader
/// binds too.
fn label_values(input_schema: &'static Schema, path: &str) -> Vec<(&'static str, String)> {
    let Some(pattern) = input_schema.http().map(|http| http.uri()) else {
        return Vec::new();
    };
    // A route pattern may carry static query literals used only to disambiguate overloaded
    // routes; only its path portion binds a label.
    let pattern = pattern.split('?').next().unwrap_or(pattern);
    let templates = segments_of(pattern);
    let supplied = segments_of(path);
    let mut bound: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0_usize;
    for template in templates {
        let Some(label) = template
            .strip_prefix('{')
            .and_then(|inner| inner.strip_suffix('}'))
        else {
            // A literal segment: the path has to carry exactly it, or nothing past this
            // point is bound.
            if supplied.get(at) != Some(&template) {
                return bound;
            }
            at += 1;
            continue;
        };
        let (name, greedy) = match label.strip_suffix('+') {
            Some(name) => (name, true),
            None => (label, false),
        };
        let value = if greedy {
            let rest = supplied.get(at..).unwrap_or_default();
            if rest.is_empty() {
                return bound;
            }
            at = supplied.len();
            rest.join("/")
        } else {
            match supplied.get(at) {
                Some(segment) => {
                    at += 1;
                    (*segment).to_string()
                }
                None => return bound,
            }
        };
        // A label the input shape declares no member for binds nothing, but its segments
        // are consumed above all the same, so the labels after it stay aligned.
        if let Some(member) = member_named(input_schema, name) {
            bound.push((member, ::service_common::percent_decode_path_label(&value)));
        }
    }
    bound
}

/// The segments of a path or of a URI pattern, with the leading and trailing `/` dropped —
/// the same splitting the framework's own matcher does, including the empty path having no
/// segments at all.
fn segments_of(path: &str) -> Vec<&str> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed.split('/').collect()
}

/// The name of the member `input_schema` declares under `name`, as the member itself
/// spells it — the key a label is bound under, and the key the framework's reader looks a
/// label up by.
fn member_named(input_schema: &'static Schema, name: &str) -> Option<&'static str> {
    input_schema
        .members()
        .iter()
        .filter_map(|member| member.member_name())
        .find(|member| *member == name)
}

/// Whether `value` converts to the modelled type `target`, by the same rule the framework's
/// single-value reader applies: exactly `true` or `false` for a boolean, and `str::parse`
/// at the modelled width for a number. Anything this layer does not answer for converts
/// vacuously — it is not this layer's refusal to make.
fn converts(value: &str, target: ShapeType) -> bool {
    match target {
        ShapeType::Boolean => matches!(value, "true" | "false"),
        ShapeType::Byte => value.parse::<i8>().is_ok(),
        ShapeType::Short => value.parse::<i16>().is_ok(),
        ShapeType::Integer => value.parse::<i32>().is_ok(),
        ShapeType::Long => value.parse::<i64>().is_ok(),
        ShapeType::Float => value.parse::<f32>().is_ok(),
        ShapeType::Double => value.parse::<f64>().is_ok(),
        _ => true,
    }
}

/// The name a conversion failure calls `target` by, and so also WHICH targets this layer
/// answers for: the scalar types a URI value is converted into. `None` for every other
/// modelled type — a string, a timestamp, a blob, an enum, a list — whose refusal this
/// layer leaves to the framework rather than inventing a sentence for.
fn converted_type_name(target: ShapeType) -> Option<&'static str> {
    match target {
        ShapeType::Boolean => Some("Boolean"),
        ShapeType::Byte => Some("Byte"),
        ShapeType::Short => Some("Short"),
        ShapeType::Integer => Some("Integer"),
        ShapeType::Long => Some("Long"),
        ShapeType::Float => Some("Float"),
        ShapeType::Double => Some("Double"),
        _ => None,
    }
}

/// The sentence a failed conversion states: the value the request sent, quoted, and the
/// type it could not be converted to — the two facts a caller has to reconcile, and the
/// two the refusal is built from.
///
/// Built from the REQUEST, which is why a recorded example of it
/// (`'latest' can not be converted to Long`) is one instance of the sentence rather than
/// the sentence itself. The sentence FRAME is the one every service in this tree words an
/// unreadable value with (`service_common::json_wire_values::unreadable_message`); the echo
/// is held to the one ceiling this tree quotes a request value up to, so an oversized path
/// segment cannot make the error body as large as itself.
fn unconvertible_message(value: &str, named: &str) -> String {
    ::service_common::json_wire_values::unreadable_message(
        &format!("'{}'", ::service_common::validation::quoted_echo(value)),
        named,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::aws_smithy_schema::codec::Codec;
    use ::aws_smithy_schema_server::json::JsonCodec;
    use ::aws_smithy_schema_server::HttpBindingDeserializer;
    use ::smithy_server_runtime::DeserializableStruct;

    /// Where `GetLayerVersion` is bound, as its own `@http` trait states it.
    const GET_LAYER_VERSION_URI: &str = "/2018-10-31/layers/{LayerName}/versions/{VersionNumber}";

    /// Whether the FRAMEWORK's own reader binds `path` to `GetLayerVersion`'s input.
    ///
    /// Drives the vendored `HttpBindingDeserializer` over that operation's real input
    /// schema and generated input shape — the same reader, the same schema and the same
    /// `DeserializableStruct` the dispatch sequence uses — so its verdict here IS the
    /// verdict this layer has to agree with. The body is an empty object because every
    /// member of that input is bound into the URI.
    fn reads_through_the_framework(path: &str) -> bool {
        let codec = JsonCodec::rest_json();
        let body = codec.create_deserializer(b"{}");
        let mut deserializer = HttpBindingDeserializer::new(
            Some(GET_LAYER_VERSION_URI),
            path,
            "",
            Vec::new(),
            body,
        );
        crate::ops::types::GetLayerVersionInput::deserialize(&mut deserializer).is_ok()
    }

    /// The agreement this layer stands on: for the value bound to a numeric path label,
    /// this layer refuses exactly what the vendored reader cannot read.
    ///
    /// This is the case that makes a change to that reader
    /// (`vendor/smithy-schema-server/src/http_binding.rs`'s `StringValueDeserializer`) fail
    /// loudly here instead of quietly sending a conversion failure back to the framework's
    /// constraint envelope — or, worse, refusing a value the framework would have taken.
    /// The rows span the forms a version segment arrives in: the numbers, the width's own
    /// bound and one past it, the spellings that are no number at all, and an escape.
    #[test]
    fn this_layer_and_the_frameworks_own_reader_agree_on_every_label_value() {
        // (case, the version segment as the request spells it)
        let rows: &[(&str, &str)] = &[
            ("a version number", "1"),
            ("a large version number", "9999"),
            ("the largest i64", "9223372036854775807"),
            ("one past the largest i64", "9223372036854775808"),
            ("a negative version number", "-1"),
            ("a word", "latest"),
            ("a fractional number", "1.5"),
            ("a percent-escaped digit", "%31"),
        ];
        for (case, segment) in rows {
            let path = format!("/2018-10-31/layers/probe/versions/{segment}");
            let framework = reads_through_the_framework(&path);
            let refused =
                refusal_for(crate::ops::types::GET_LAYER_VERSION_INPUT_SCHEMA, &path).is_some();
            assert_eq!(
                refused,
                !framework,
                "case {case:?} ({segment:?}): this layer refuses == {refused}, the \
                 framework's own reader reads it == {framework}"
            );
        }

        // Non-vacuity: the rows must cover BOTH answers, or the agreement proves nothing.
        assert!(
            rows.iter().any(|(_, segment)| reads_through_the_framework(
                &format!("/2018-10-31/layers/probe/versions/{segment}")
            )) && rows.iter().any(|(_, segment)| !reads_through_the_framework(
                &format!("/2018-10-31/layers/probe/versions/{segment}")
            )),
            "the rows must span both readable and unreadable label values"
        );
    }

    /// A label is bound to the member the pattern names it for, and nothing is bound for a
    /// path that does not match the pattern.
    #[test]
    fn a_label_is_bound_to_the_member_the_pattern_names() {
        // (case, the path, the version the walk bound, if any)
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "the modelled path",
                "/2018-10-31/layers/probe/versions/7",
                Some("7"),
            ),
            (
                "a percent-escaped segment",
                "/2018-10-31/layers/probe/versions/%37",
                Some("7"),
            ),
            (
                "a value that is no number",
                "/2018-10-31/layers/probe/versions/latest",
                Some("latest"),
            ),
            (
                "another operation's path",
                "/2018-10-31/layers/probe/versions",
                None,
            ),
            ("no path at all", "/", None),
            (
                "a path under another API date",
                "/2015-03-31/functions/probe/versions/7",
                None,
            ),
        ];
        for (case, path, expected) in rows.iter().copied() {
            let bound = label_values(crate::ops::types::GET_LAYER_VERSION_INPUT_SCHEMA, path);
            let version = bound
                .iter()
                .find(|(member, _)| *member == "VersionNumber")
                .map(|(_, value)| value.clone());
            assert_eq!(
                version.as_deref(),
                expected,
                "case {case:?} ({path:?}): expected the version label {expected:?}, got \
                 {version:?}"
            );
        }
    }

    /// The conversion rule, over the vocabulary of a URI value: the boolean words, the
    /// numeric widths and one value past each width's bound.
    #[test]
    fn a_uri_value_converts_exactly_where_its_modelled_type_accepts_it() {
        // (case, value, target, converts)
        let rows: &[(&str, &str, ShapeType, bool)] = &[
            ("the boolean true", "true", ShapeType::Boolean, true),
            ("the boolean false", "false", ShapeType::Boolean, true),
            ("a capitalised boolean", "True", ShapeType::Boolean, false),
            ("a numeric boolean", "1", ShapeType::Boolean, false),
            ("an empty boolean", "", ShapeType::Boolean, false),
            ("an integer", "25", ShapeType::Integer, true),
            ("a negative integer", "-1", ShapeType::Integer, true),
            ("the largest i32", "2147483647", ShapeType::Integer, true),
            (
                "one past the largest i32",
                "2147483648",
                ShapeType::Integer,
                false,
            ),
            ("a fractional integer", "2.5", ShapeType::Integer, false),
            ("a word as an integer", "lots", ShapeType::Integer, false),
            ("the largest i8", "127", ShapeType::Byte, true),
            ("one past the largest i8", "128", ShapeType::Byte, false),
            ("the largest i16", "32767", ShapeType::Short, true),
            ("one past the largest i16", "32768", ShapeType::Short, false),
            ("a long", "9223372036854775807", ShapeType::Long, true),
            (
                "one past the largest i64",
                "9223372036854775808",
                ShapeType::Long,
                false,
            ),
            ("an empty long", "", ShapeType::Long, false),
            ("a double", "2.5", ShapeType::Double, true),
            ("a word as a double", "lots", ShapeType::Double, false),
            // A type this layer does not answer for is never refused here.
            ("a string", "anything", ShapeType::String, true),
            ("a timestamp", "half past four", ShapeType::Timestamp, true),
        ];
        for (case, value, target, expected) in rows.iter().copied() {
            let actual = converts(value, target);
            assert_eq!(
                actual, expected,
                "case {case:?} ({value:?} as {target:?}): expected converts == {expected}, \
                 got {actual}"
            );
        }
    }

    /// The sentence names the value as the request spelled it and the type it could not
    /// become, with no article in front of the type — the form the framework's envelope
    /// carries on this surface.
    #[test]
    fn the_sentence_states_the_value_and_the_type() {
        // (case, the value, the type's name, the sentence)
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "a word where a long is read",
                "latest",
                "Long",
                "'latest' can not be converted to Long",
            ),
            (
                "a word where an integer is read",
                "lots",
                "Integer",
                "'lots' can not be converted to Integer",
            ),
            ("an empty value", "", "Long", "'' can not be converted to Long"),
        ];
        for (case, value, named, expected) in rows.iter().copied() {
            let stated = unconvertible_message(value, named);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}
