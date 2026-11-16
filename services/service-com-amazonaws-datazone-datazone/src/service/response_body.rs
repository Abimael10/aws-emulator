// SPDX-License-Identifier: CC0-1.0

//! Replacing the BODY of a response this service has already rendered.
//!
//! Two seams of this service restate a body the framework renderer wrote: what a FAILURE
//! carries ([`super::rest_json_error_body`]) and the member name a domain listing's summaries
//! are served under ([`super::recorded_response_members`]). Both hand back a response whose
//! body is not the bytes its renderer produced, and both owe the same thing to the transport —
//! a `content-length` that describes what is actually being sent — so that framing concern is
//! stated once here rather than in each of them.

use aws_smithy_runtime_api::http::Response;
use aws_smithy_types::body::SdkBody;

/// `resp` carrying `body` instead of the bytes its renderer wrote, with a `content-length` the
/// renderer advertised corrected to describe it.
#[must_use]
pub(crate) fn with_body(mut resp: Response, body: Vec<u8>) -> Response {
    // Only a renderer that advertised a length needs it corrected; the framework's renderers
    // set `content-type` alone and leave framing to the transport.
    if resp.headers().get("content-length").is_some() {
        let length = body.len().to_string();
        // A decimal length is always a valid header value, so this cannot fail today; if it
        // ever did, the header is REMOVED rather than left describing the body before the
        // rewrite, so the two can never disagree.
        if resp
            .headers_mut()
            .try_insert("content-length", length)
            .is_err()
        {
            resp.headers_mut().remove("content-length");
        }
    }
    *resp.body_mut() = SdkBody::from(body);
    resp
}

#[cfg(test)]
mod tests {
    use super::with_body;
    use aws_smithy_types::body::SdkBody;

    /// A response carrying `body`, with `advertised` deciding whether its renderer set a
    /// `content-length` at all.
    fn response(body: &str, advertised: bool) -> aws_smithy_runtime_api::http::Response {
        let mut resp = aws_smithy_runtime_api::http::Response::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(200).expect("a valid status"),
            SdkBody::from(body),
        );
        if advertised {
            resp.headers_mut()
                .insert("content-length", body.len().to_string());
        }
        resp
    }

    /// RULE: the replacement body is what the response carries, and a length the renderer
    /// advertised describes it — while a renderer that advertised none still advertises none.
    /// The rows differ along whether a length was advertised and whether the new body is
    /// longer, shorter or empty.
    #[test]
    fn a_replaced_body_is_described_by_the_length_its_renderer_advertised() {
        for (case, rendered, advertised, replacement) in [
            ("a longer body, length advertised", "{}", true, r#"{"a":1}"#),
            ("a shorter body, length advertised", r#"{"a":1}"#, true, "{}"),
            ("an empty body, length advertised", r#"{"a":1}"#, true, ""),
            ("a longer body, no length advertised", "{}", false, r#"{"a":1}"#),
        ] {
            let answered = with_body(
                response(rendered, advertised),
                replacement.as_bytes().to_vec(),
            );
            assert_eq!(
                answered.body().bytes().expect("bytes"),
                replacement.as_bytes(),
                "case {case:?}: the response must carry the replacement body"
            );
            let length = answered.headers().get("content-length");
            let expected = advertised.then(|| replacement.len().to_string());
            assert_eq!(
                length,
                expected.as_deref(),
                "case {case:?}: expected content-length {expected:?}, got {length:?}"
            );
        }
    }
}
