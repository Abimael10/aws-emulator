// SPDX-License-Identifier: CC0-1.0

//! WHICH part of the object a viewer asking for a byte range is answered with: the window it
//! selected, the refusal of a range the object cannot satisfy, or the whole object.
//!
//! The RULE — what a `Range` header asks of a representation of a given size — is HTTP's, read
//! in one place for the whole tree ([`::service_common::byte_range`]). What is CloudFront's
//! own, and so lives here, is the pair of decisions around it: WHICH of the answers the edge
//! produces has a window to take at all, and HOW that window (or that refusal) is served.
//!
//! The edge evaluates the range itself, against the representation it is about to serve — the
//! entry its cache held, or the answer the origin has just produced:
//!
//! * a range that selects bytes of it is answered `206 Partial Content` carrying exactly those
//!   bytes, with `Content-Range: bytes <first>-<last>/<total>` and a `Content-Length` of the
//!   window;
//! * a range that selects none of it is refused `416 Range Not Satisfiable` with
//!   `Content-Range: bytes */<total>` and NONE of the object's bytes. Serving the whole object
//!   to such a viewer would be the over-permissive answer, so it is the one case that must be
//!   refused rather than widened;
//! * a `Range` the reading does not act on is ignored, and the complete object is served.
//!
//! Range handling is defined for `GET` alone (RFC 9110, "Range Requests": "a server MUST ignore
//! a Range header field received with a request method that is unrecognized"), so no other
//! method's answer is sliced.
//!
//! The window is taken from the REQUEST and the length of the representation the edge is
//! serving, so one object answers every window a viewer asks for, and nothing here depends on
//! what any one object happens to contain.

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::aws_smithy_types::body::SdkBody;
use ::service_common::byte_range::{requested_range, unsatisfied_content_range, RangeRequest};

use super::status_code;

/// The response header a partial answer states its window and the object's full length in.
const CONTENT_RANGE: &str = "content-range";

/// The response header stating how many bytes the answer carries, which a partial answer
/// states as the size of its window.
const CONTENT_LENGTH: &str = "content-length";

/// The response header stating what coding the bytes are under — which a refusal, carrying no
/// bytes at all, does not state.
const CONTENT_ENCODING: &str = "content-encoding";

/// The request header a viewer asks for part of an object in.
pub const RANGE: &str = "range";

/// The method range handling is defined for.
const RANGED_METHOD: &str = "GET";

/// The status an answer carrying the WHOLE representation has — the only answer a range is
/// evaluated against, since a partial answer carries no complete object to take a window of.
const COMPLETE: u16 = 200;

/// The status of an answer carrying the window a satisfiable range selected.
const PARTIAL_CONTENT: u16 = 206;

/// The status of a refusal of a range that selects none of the representation.
const RANGE_NOT_SATISFIABLE: u16 = 416;

/// Answer `response` with the window the viewer's `range` selects, when it is a range the edge
/// acts on.
///
/// `response` is the answer the edge is about to serve on the origin's behalf — the cache's
/// entry or what the origin produced — so the window is taken from the representation the
/// viewer would otherwise have received whole. A request carrying no `Range`, a method range
/// handling is not defined for, and an answer that is not one complete representation are all
/// left exactly as they are.
pub fn apply(range: Option<&str>, method: &str, response: &mut RtResponse) {
    if !method.eq_ignore_ascii_case(RANGED_METHOD) {
        return;
    }
    let Some(range) = range else {
        return;
    };
    // Only ONE complete representation has a window to take: an answer that is already partial
    // (an origin that evaluated a range itself) states its own, and a status other than `200`
    // is no representation of the object at all.
    if response.status().as_u16() != COMPLETE || response.headers().get(CONTENT_RANGE).is_some() {
        return;
    }
    let Some(body) = response.body().bytes() else {
        return;
    };
    let total = body.len();
    match requested_range(range, total) {
        RangeRequest::Ignored => {}
        RangeRequest::Span(span) => {
            // The span's own reading of the body, which is clamped to it, so no window can read
            // past the bytes the edge holds.
            let window = span.slice(body);
            let length = window.len();
            *response = rebuilt(response, PARTIAL_CONTENT, window);
            let _ = response
                .headers_mut()
                .try_insert(CONTENT_RANGE, span.content_range(total));
            let _ = response
                .headers_mut()
                .try_insert(CONTENT_LENGTH, length.to_string());
        }
        RangeRequest::Unsatisfiable => {
            // NONE of the object's bytes: the refusal states how long the representation is and
            // carries nothing of it. The coding the ANSWER's bytes were under goes with them,
            // since a body that is not there is under no coding.
            *response = rebuilt(response, RANGE_NOT_SATISFIABLE, Vec::new());
            response.headers_mut().remove(CONTENT_ENCODING);
            let _ = response
                .headers_mut()
                .try_insert(CONTENT_RANGE, unsatisfied_content_range(total));
            let _ = response
                .headers_mut()
                .try_insert(CONTENT_LENGTH, "0".to_string());
        }
    }
}

/// `response` under a new status and body, keeping every header it carries — the origin's own
/// and the ones the edge stamped — so a partial answer describes the same object the whole one
/// did.
fn rebuilt(response: &RtResponse, status: u16, body: Vec<u8>) -> RtResponse {
    let mut rebuilt = RtResponse::new(status_code(status), SdkBody::from(body));
    for (name, value) in response.headers().iter() {
        let _ = rebuilt
            .headers_mut()
            .try_append(name.to_string(), value.to_string());
    }
    rebuilt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer a satisfiable range is served with: the window's bytes, the status, and the
    /// two headers that state the window — over the headers the whole answer carried.
    #[test]
    fn a_satisfiable_range_is_answered_with_the_window_alone() {
        let object: Vec<u8> = (0..=255u8).collect();
        let mut response = RtResponse::new(status_code(COMPLETE), SdkBody::from(object.clone()));
        let _ = response
            .headers_mut()
            .try_insert("x-cache", "Hit from cloudfront".to_string());

        apply(Some("bytes=10-19"), "GET", &mut response);

        assert_eq!(
            response.status().as_u16(),
            PARTIAL_CONTENT,
            "expected {PARTIAL_CONTENT}, got {}",
            response.status().as_u16()
        );
        assert_eq!(
            response.body().bytes(),
            Some(&object[10..=19]),
            "expected exactly bytes 10..=19, got {:?}",
            response.body().bytes()
        );
        assert_eq!(
            response.headers().get(CONTENT_RANGE),
            Some("bytes 10-19/256"),
            "expected the window and the object's length, got {:?}",
            response.headers().get(CONTENT_RANGE)
        );
        assert_eq!(
            response.headers().get(CONTENT_LENGTH),
            Some("10"),
            "expected the size of the window, got {:?}",
            response.headers().get(CONTENT_LENGTH)
        );
        assert_eq!(
            response.headers().get("x-cache"),
            Some("Hit from cloudfront"),
            "expected the answer's own cache outcome to be kept, got {:?}",
            response.headers().get("x-cache")
        );
    }

    /// An unsatisfiable range carries none of the object's bytes, and a method range handling
    /// is not defined for is served whole.
    #[test]
    fn an_unsatisfiable_range_is_refused_and_another_method_is_served_whole() {
        let object: Vec<u8> = (0..=255u8).collect();
        let mut refused = RtResponse::new(status_code(COMPLETE), SdkBody::from(object.clone()));
        apply(Some("bytes=256-300"), "GET", &mut refused);
        assert_eq!(
            refused.status().as_u16(),
            RANGE_NOT_SATISFIABLE,
            "expected {RANGE_NOT_SATISFIABLE}, got {}",
            refused.status().as_u16()
        );
        assert_eq!(
            refused.body().bytes().map(<[u8]>::len),
            Some(0),
            "expected none of the object's bytes, got {:?} bytes",
            refused.body().bytes().map(<[u8]>::len)
        );
        assert_eq!(
            refused.headers().get(CONTENT_RANGE),
            Some("bytes */256"),
            "expected the object's length alone, got {:?}",
            refused.headers().get(CONTENT_RANGE)
        );
        assert_eq!(
            refused.headers().get(CONTENT_LENGTH),
            Some("0"),
            "expected a Content-Length of nothing, got {:?}",
            refused.headers().get(CONTENT_LENGTH)
        );

        let mut head = RtResponse::new(status_code(COMPLETE), SdkBody::from(object.clone()));
        apply(Some("bytes=10-19"), "HEAD", &mut head);
        assert_eq!(
            (head.status().as_u16(), head.body().bytes().map(<[u8]>::len)),
            (COMPLETE, Some(object.len())),
            "expected a method range handling is not defined for to be served whole, got {} \
             with {:?} bytes",
            head.status().as_u16(),
            head.body().bytes().map(<[u8]>::len)
        );
    }

    /// A refusal carries no bytes, so it states no coding for them — the answer's own
    /// `Content-Encoding` goes with the body it described.
    #[test]
    fn a_refusal_states_no_coding_for_a_body_it_does_not_carry() {
        let object: Vec<u8> = (0..=255u8).collect();
        let mut refused = RtResponse::new(status_code(COMPLETE), SdkBody::from(object));
        let _ = refused
            .headers_mut()
            .try_insert(CONTENT_ENCODING, "gzip".to_string());
        apply(Some("bytes=1024-2047"), "GET", &mut refused);
        assert_eq!(
            (
                refused.status().as_u16(),
                refused.headers().get(CONTENT_ENCODING)
            ),
            (RANGE_NOT_SATISFIABLE, None),
            "expected the refusal to state no coding, got {} with {:?}",
            refused.status().as_u16(),
            refused.headers().get(CONTENT_ENCODING)
        );
    }

    /// An answer that is not one complete representation is left alone: a page CloudFront
    /// generated itself, and an answer that already states its own `Content-Range`.
    #[test]
    fn an_answer_that_is_no_representation_is_left_alone() {
        let page = b"<html>CloudFront says no</html>".to_vec();
        let mut generated = RtResponse::new(status_code(403), SdkBody::from(page.clone()));
        apply(Some("bytes=0-3"), "GET", &mut generated);
        assert_eq!(
            (
                generated.status().as_u16(),
                generated.body().bytes().map(<[u8]>::len)
            ),
            (403, Some(page.len())),
            "expected the generated page untouched, got {} with {:?} bytes",
            generated.status().as_u16(),
            generated.body().bytes().map(<[u8]>::len)
        );

        let object: Vec<u8> = (0..=255u8).collect();
        let mut already_partial =
            RtResponse::new(status_code(COMPLETE), SdkBody::from(object.clone()));
        let _ = already_partial
            .headers_mut()
            .try_insert(CONTENT_RANGE, "bytes 0-255/512".to_string());
        apply(Some("bytes=0-3"), "GET", &mut already_partial);
        assert_eq!(
            already_partial.headers().get(CONTENT_RANGE),
            Some("bytes 0-255/512"),
            "expected the answer's own Content-Range kept, got {:?}",
            already_partial.headers().get(CONTENT_RANGE)
        );
        assert_eq!(
            already_partial.body().bytes().map(<[u8]>::len),
            Some(object.len()),
            "expected the answer's own bytes kept, got {:?}",
            already_partial.body().bytes().map(<[u8]>::len)
        );
    }
}
