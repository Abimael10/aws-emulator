// SPDX-License-Identifier: CC0-1.0

//! What the API GATEWAY integration takes from this module's reading of an ANSWER
//! ([`crate::state::integration::api_gateway`]).
//!
//! Both integrations whose unit of work is one HTTP request share this module: the request
//! goes over its transport ([`super::dispatch`]) and a served answer is reported through
//! its reading ([`super::reported`]). What the API Gateway integration needs BESIDE that
//! reporting is the answer's own BODY, because the cause its failed invoke carries is that
//! body and nothing else — and the bytes it is read from are [`super::Answered`]'s, which
//! this module's parent owns. So the one reading of those bytes AS TEXT lives here, beside
//! them, rather than in a second place that would have to reach for them.

use super::Answered;

/// The BODY `answered` carried, as the text it carried it in.
///
/// Lossy on purpose: the bytes are an endpoint's own and a cause carries TEXT, so a body
/// that is not valid UTF-8 is read as the text its bytes stand for rather than costing the
/// cause altogether — what a `Catch` path is told is still what the endpoint sent.
pub(in crate::state) fn answered_body(answered: &Answered) -> String {
    String::from_utf8_lossy(&answered.payload).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body is read as the text the endpoint sent, whatever it sent. The rows differ
    /// along the form those bytes take: a JSON document, a sentence, no body at all, and
    /// bytes that are not UTF-8.
    #[test]
    fn the_body_is_read_as_the_text_the_endpoint_sent() {
        let cases: &[(&str, &[u8], &str)] = &[
            (
                "the document a backend answered",
                br#"{"marker":"err500"}"#,
                r#"{"marker":"err500"}"#,
            ),
            (
                "the sentence a gateway response answered",
                br#"{"message":"Forbidden"}"#,
                r#"{"message":"Forbidden"}"#,
            ),
            ("an answer with no body at all", b"", ""),
            // The replacement character the lossy reading stands a broken sequence for.
            ("bytes that are no text", &[0x7b, 0xff, 0x7d], "{\u{fffd}}"),
        ];
        for (case, payload, expected) in cases {
            let answered = Answered {
                status: 500,
                headers: Vec::new(),
                payload: payload.to_vec(),
            };
            let read = answered_body(&answered);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
