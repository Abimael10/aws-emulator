// SPDX-License-Identifier: CC0-1.0

//! `contentHandling`: which FORM a payload travels in — text, or the bytes that text stands
//! for — on the integration request and on the method response.
//!
//! The form a payload is in is the api's own record to say: `RestApi$binaryMediaTypes` is
//! "the list of binary media types supported by the RestApi. By default, the RestApi supports
//! only UTF-8-encoded text payloads", so a payload whose media type the api declares binary IS
//! the binary form and every other payload is the text form. It is read with the api's OWN
//! reading of that member ([`super::lambda_proxy::declares_binary`]), so the media types this
//! module calls binary and the ones a proxy integration's base64 body is decoded for cannot
//! disagree.
//!
//! The form WANTED is the one the call NAMES: the method request's `Content-Type` for the
//! integration request, and the request's `Accept` for the method response — which is what
//! makes an api that declares a media type binary serve the BYTES a text payload stands for to
//! a client that asks for that type, and the base64 TEXT of a binary one to a client that asks
//! for a type it declares nothing about. A request that names no `Accept` at all accepts
//! whatever the integration answered, so its payload travels in the form it is already in.
//! `Integration$contentHandling` and `IntegrationResponse$contentHandling` OVERRIDE that:
//! "`CONVERT_TO_BINARY`: Converts a request payload from a base64-encoded string to the
//! corresponding binary blob" and "`CONVERT_TO_TEXT`: Converts a request payload from a binary
//! blob to a base64-encoded string". With neither stated an integration request's payload is
//! "passed through from the method request to the integration request without modification",
//! which is what the rule above gives it: the media type that names the form wanted is the
//! very one the payload arrived under.
//!
//! Moving between the two forms is the one conversion each way: the bytes a text payload
//! stands for are its base64 decoded, and the text a binary payload stands for is its base64
//! encoded. A payload already in the form wanted travels unmodified, so neither conversion is
//! performed twice and neither is performed on a payload that needs none.
//!
//! Text the api wants as bytes and that is not base64 at all decodes to nothing: the
//! integration cannot be performed over it, so the call is answered as the configuration error
//! it is rather than with a payload built from part of the text.

use super::*;

/// "CONVERT_TO_BINARY: Converts a request payload from a base64-encoded string to the
/// corresponding binary blob."
const CONVERT_TO_BINARY: &str = "CONVERT_TO_BINARY";

/// "CONVERT_TO_TEXT: Converts a request payload from a binary blob to a base64-encoded
/// string."
const CONVERT_TO_TEXT: &str = "CONVERT_TO_TEXT";

/// Which of the two forms a payload is in, or is wanted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The payload is text — the default, since an api "supports only UTF-8-encoded text
    /// payloads" until it declares a media type binary.
    Text,
    /// The payload is the bytes that text stands for.
    Binary,
}

/// `payload` in the form this call wants it, or the configuration error a payload that cannot
/// be converted to it earns.
///
/// `carried` is the payload's own media type — the `Content-Type` the method request declared
/// for an integration request, and the one the integration answered for a method response —
/// and `wanted` the media type that names the form wanted: the same `Content-Type` for an
/// integration request, and the request's `Accept` for a method response, or `None` for a
/// request that accepts anything. `stated` is the `contentHandling` that overrides it, and
/// `binary_media_types` the api's own `binaryMediaTypes`.
///
/// `named` is what the payload is, for the sentence a refusal carries.
pub(super) fn converted(
    payload: Vec<u8>,
    stated: Option<&str>,
    carried: &str,
    wanted: Option<&str>,
    binary_media_types: &[String],
    named: &str,
) -> Result<Vec<u8>, UnservedIntegration> {
    let form_of = |media: &str| {
        if lambda_proxy::declares_binary(media, binary_media_types) {
            Form::Binary
        } else {
            Form::Text
        }
    };
    let target = match stated.filter(|stated| !stated.is_empty()) {
        Some(CONVERT_TO_BINARY) => Form::Binary,
        Some(CONVERT_TO_TEXT) => Form::Text,
        // Nothing stated: the form the call NAMES, and the form the payload is already in
        // where the call names none — a request that states no `Accept` accepts whatever the
        // integration answered, so nothing is converted for it.
        _ => wanted.map_or_else(|| form_of(carried), form_of),
    };
    match (form_of(carried), target) {
        (Form::Text, Form::Binary) => decoded(&payload).ok_or_else(|| UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: format!(
                "the integration carries {named} as the {} bytes the api wants as binary, and \
                 the {carried} payload is not the base64 text such a conversion decodes",
                payload.len()
            ),
        }),
        (Form::Binary, Form::Text) => Ok(::base64::Engine::encode(
            &::base64::engine::general_purpose::STANDARD,
            &payload,
        )
        .into_bytes()),
        // Already in the form wanted: it travels unmodified.
        _ => Ok(payload),
    }
}

/// The bytes the base64 text `payload` stands for, or `None` for text that is not that
/// encoding at all.
///
/// By the maintained coder for the format, over the standard alphabet and padding RFC 4648
/// states — the same encoding every other base64 value of this plane is read in.
fn decoded(payload: &[u8]) -> Option<Vec<u8>> {
    ::base64::Engine::decode(&::base64::engine::general_purpose::STANDARD, payload).ok()
}

/// What the INTEGRATION REQUEST's payload is called in the sentence a refusal carries.
pub(super) const THE_INTEGRATION_REQUEST: &str = "the integration request's payload";

/// What the METHOD RESPONSE's payload is called in that sentence.
pub(super) const THE_METHOD_RESPONSE: &str = "the method response's payload";

#[cfg(test)]
mod tests {
    use super::*;

    /// The api of the cases below declares one media type binary, which is what decides which
    /// form a payload is in and which one a call wants.
    fn binary_media_types() -> Vec<String> {
        vec!["application/octet-stream".to_string()]
    }

    /// RULE: a payload is converted to the form the call wants it in — `contentHandling`
    /// where it states one, and the media type the call names otherwise — and a payload
    /// already in that form travels unmodified.
    #[test]
    fn a_payload_travels_in_the_form_the_call_wants() {
        /// (case, the conversion stated, the payload's media type, the media type naming the
        /// form wanted, the payload, what the conversion makes of it)
        type Row<'a> = (
            &'a str,
            Option<&'a str>,
            &'a str,
            Option<&'a str>,
            &'a [u8],
            &'a [u8],
        );
        let rows: &[Row<'_>] = &[
            (
                "CONVERT_TO_BINARY over a text payload decodes it",
                Some(CONVERT_TO_BINARY),
                "text/plain",
                None,
                b"AAECAw==",
                &[0, 1, 2, 3],
            ),
            (
                "CONVERT_TO_BINARY over a payload the api declares binary passes it through",
                Some(CONVERT_TO_BINARY),
                "application/octet-stream",
                None,
                b"AAECAw==",
                b"AAECAw==",
            ),
            (
                "CONVERT_TO_TEXT over a payload the api declares binary encodes it",
                Some(CONVERT_TO_TEXT),
                "application/octet-stream",
                None,
                &[0, 1, 2, 3],
                b"AAECAw==",
            ),
            (
                "CONVERT_TO_TEXT over a text payload passes it through",
                Some(CONVERT_TO_TEXT),
                "text/plain",
                None,
                b"plain words",
                b"plain words",
            ),
            (
                "nothing stated, and the form wanted is the one the payload is in",
                None,
                "text/plain",
                Some("text/plain"),
                b"AAECAw==",
                b"AAECAw==",
            ),
            (
                "nothing stated over a binary payload the call wants as binary",
                None,
                "application/octet-stream",
                Some("application/octet-stream"),
                &[0, 1, 2, 3],
                &[0, 1, 2, 3],
            ),
            (
                "nothing stated, and the call wants as binary a payload that is text",
                None,
                "text/plain",
                Some("application/octet-stream"),
                b"AAECAw==",
                &[0, 1, 2, 3],
            ),
            (
                "nothing stated, and the call names text for a payload that is binary",
                None,
                "application/octet-stream",
                Some("text/plain"),
                &[0, 1, 2, 3],
                b"AAECAw==",
            ),
            (
                "nothing stated and the call naming no form at all, which moves nothing",
                None,
                "application/octet-stream",
                None,
                &[0, 1, 2, 3],
                &[0, 1, 2, 3],
            ),
            (
                "the media types are matched without the parameters they may carry",
                Some(CONVERT_TO_TEXT),
                "application/octet-stream; charset=binary",
                None,
                &[0, 1, 2, 3],
                b"AAECAw==",
            ),
            (
                "an empty payload converts to an empty one",
                Some(CONVERT_TO_BINARY),
                "text/plain",
                None,
                b"",
                b"",
            ),
        ];
        for (case, stated, carried, wanted, payload, expected) in rows {
            let actual = converted(
                payload.to_vec(),
                *stated,
                carried,
                *wanted,
                &binary_media_types(),
                THE_INTEGRATION_REQUEST,
            );
            assert_eq!(
                actual.as_deref().ok(),
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: text the call wants as bytes and that is not base64 is the configuration error of
    /// an integration that cannot be performed, not a payload built from part of it.
    #[test]
    fn text_that_is_not_base64_cannot_be_carried_as_bytes() {
        for payload in [
            &b"not base64 at all"[..],
            &b"AAEC*"[..],
            &b"AAECA"[..],
            &b"=="[..],
        ] {
            for (stated, wanted) in [
                (Some(CONVERT_TO_BINARY), None),
                (None, Some("application/octet-stream")),
            ] {
                let actual = converted(
                    payload.to_vec(),
                    stated,
                    "text/plain",
                    wanted,
                    &binary_media_types(),
                    THE_METHOD_RESPONSE,
                );
                assert!(
                    actual.is_err(),
                    "case {payload:?} with {stated:?}/{wanted:?}: expected the conversion to \
                     be refused, got {actual:?}"
                );
            }
        }
    }
}
