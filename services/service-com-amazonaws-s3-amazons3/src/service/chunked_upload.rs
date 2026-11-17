// SPDX-License-Identifier: CC0-1.0

//! The `aws-chunked` FRAMING a write's body arrived in, and what a body that declares it
//! without saying how long its payload is earns.
//!
//! An SDK that streams an object's bytes sends them in the `aws-chunked` transfer encoding:
//! `content-encoding: aws-chunked`, the payload as a sequence of length-prefixed chunks, and
//! `x-amz-decoded-content-length` naming how many bytes those chunks decode to. That last
//! header is not optional — it is how S3 knows the LENGTH of the object it is being asked to
//! store, which a framed body does not state anywhere else — so a chunked write that leaves
//! it off is refused `411` [`MISSING_CONTENT_LENGTH_CODE`], exactly as an unframed write that
//! states no `Content-Length` is, and no object is created.
//!
//! The framing itself is a TRANSPORT concern the core strips off before any operation of this
//! service is reached: by the time a handler runs, the body holds the payload, `content-length`
//! reports its length and the two framing headers are gone — which is why WHAT THE REQUEST
//! SENT is read here, at the one seam this service is handed the request as it arrived
//! ([`super::EmulatedService::can_handle`]), and kept for the length of that one request the
//! way the SigV4 note is ([`super::sigv4`]).
//!
//! The other half of this module is the TRAILER a framed write declares. "If you specify
//! `x-amz-trailer` … the checksum is sent in the trailing header" of the framed body, and the
//! header names WHICH checksum travels there (`x-amz-checksum-crc32` names `CRC32`), so a
//! write that declares one stores its object under that algorithm — the same record a write
//! naming `x-amz-checksum-algorithm` up front leaves, because both say the same thing about
//! the object: which checksum its reads report
//! ([`crate::state::stored_object_checksum`] computes it over the bytes stored).

use ::std::cell::RefCell;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::protocol::ServerProtocol;
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use super::wire::UnwrappedXmlRestXml;

/// The routing key the refusal below is held in the service's own table under — a key the
/// model's protocol never resolves, so no modelled request can reach it by name.
pub const REFUSE_UNFRAMED_CHUNKED_BODY: &str = "@refuse-unframed-chunked-body";

/// The header a request announces the transfer encoding of its body in.
const CONTENT_ENCODING_HEADER: &str = "content-encoding";

/// The encoding token that says the body is framed.
const AWS_CHUNKED: &str = "aws-chunked";

/// The header a framed body states its payload length in.
const DECODED_CONTENT_LENGTH_HEADER: &str = "x-amz-decoded-content-length";

/// The header a framed body names its trailing checksum in.
const TRAILER_HEADER: &str = "x-amz-trailer";

/// The prefix every checksum trailer name carries, the rest of it being the algorithm:
/// `x-amz-checksum-crc32` names `CRC32` (the `Checksum*` members' own headers).
const CHECKSUM_TRAILER_PREFIX: &str = "x-amz-checksum-";

/// The error code S3 refuses a write that does not say how long its payload is with, and the
/// sentence its published error list states for that code.
pub const MISSING_CONTENT_LENGTH_CODE: &str = "MissingContentLength";
/// See [`MISSING_CONTENT_LENGTH_CODE`].
pub const MISSING_CONTENT_LENGTH_MESSAGE: &str =
    "You must provide the Content-Length HTTP header.";
/// The HTTP status that code is answered with (Amazon S3 API Reference, *Error responses*:
/// `MissingContentLength`, 411 Length Required).
pub const MISSING_CONTENT_LENGTH_STATUS: u16 = 411;

thread_local! {
    /// How the body of the request this service is serving was FRAMED, for the length of that
    /// one request.
    ///
    /// Not state: a request-scoped note, needed because the framing is stripped from the
    /// request before any operation is reached, and the one seam handed the request as it
    /// ARRIVED is the claiming seam. The framework calls that seam and the routing seam within
    /// one synchronous stretch of the one dispatch, on the thread that dispatch is running on,
    /// so the note the routing seam reads is the one that dispatch recorded.
    static FRAMING: RefCell<Framing> = const { RefCell::new(Framing::UNFRAMED) };
}

/// What one request's head said about the framing of its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Framing {
    /// Whether `content-encoding` declared [`AWS_CHUNKED`].
    framed: bool,
    /// Whether the request stated [`DECODED_CONTENT_LENGTH_HEADER`].
    states_payload_length: bool,
}

impl Framing {
    /// The note for a request whose body is not framed at all, which is every request that
    /// declares no such encoding.
    const UNFRAMED: Self = Self {
        framed: false,
        states_payload_length: false,
    };
}

/// Record how `parts` framed its body, as the request this thread is serving.
///
/// Called for EVERY request this service is offered, so the note can never be read as a
/// previous request's: a request that declares no framing records that, rather than leaving
/// what the last one said.
pub(super) fn remember(parts: &::http::request::Parts) {
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let framed = header(CONTENT_ENCODING_HEADER).is_some_and(|encoding| {
        encoding
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case(AWS_CHUNKED))
    });
    let note = Framing {
        framed,
        states_payload_length: header(DECODED_CONTENT_LENGTH_HEADER).is_some(),
    };
    FRAMING.with(|framing| *framing.borrow_mut() = note);
}

/// Whether the request this thread is serving declared `aws-chunked` framing and did NOT say
/// how long its payload is — the one condition this module refuses.
pub(super) fn body_states_no_payload_length() -> bool {
    FRAMING.with(|framing| {
        let note = *framing.borrow();
        note.framed && !note.states_payload_length
    })
}

/// The `ChecksumAlgorithm` value the `x-amz-trailer` of `parts` names, or `None` for a request
/// that declares no checksum trailer.
///
/// The header carries the NAME of the trailing header the checksum arrives in, and each
/// checksum header is [`CHECKSUM_TRAILER_PREFIX`] followed by its algorithm, so the algorithm
/// is the rest of that name in the spelling the model's enum uses (upper case). A value naming
/// anything else names no algorithm this emulator computes, and nothing is read out of it.
pub fn trailer_checksum_algorithm(parts: &::http::request::Parts) -> Option<String> {
    let trailer = parts
        .headers
        .get(TRAILER_HEADER)
        .and_then(|value| value.to_str().ok())?
        .trim()
        .to_ascii_lowercase();
    let algorithm = trailer
        .strip_prefix(CHECKSUM_TRAILER_PREFIX)?
        .to_ascii_uppercase();
    crate::ops::types::CHECKSUM_ALGORITHM_VALUES
        .contains(&algorithm.as_str())
        .then_some(algorithm)
}

/// The operation that answers the refusal, holding the codec it renders the envelope with —
/// the same construction the sibling refusals of this service use
/// ([`super::invalid_bucket_name::RefuseInvalidBucketName`]).
pub struct RefuseUnframedChunkedBody {
    /// This service's own protocol: the envelope every refusal of this service goes out in.
    protocol: UnwrappedXmlRestXml,
}

impl RefuseUnframedChunkedBody {
    /// Build the refusal operation. INFALLIBLE, and it holds no state: what it answers is one
    /// code and one sentence.
    pub fn new() -> Self {
        Self {
            protocol: UnwrappedXmlRestXml::new(),
        }
    }

    /// The envelope: the published code and the sentence S3's error list states for it.
    fn refusal(&self) -> Option<RtResponse> {
        let refusal = crate::scaffolded::Unmodeled {
            code: MISSING_CONTENT_LENGTH_CODE,
            status: MISSING_CONTENT_LENGTH_STATUS,
            message: MISSING_CONTENT_LENGTH_MESSAGE.to_string(),
        };
        self.protocol
            .serialize_error(
                &refusal,
                &crate::scaffolded::UNMODELED_SCHEMA,
                MISSING_CONTENT_LENGTH_CODE,
                MISSING_CONTENT_LENGTH_STATUS,
            )
            .ok()
    }
}

impl Default for RefuseUnframedChunkedBody {
    fn default() -> Self {
        Self::new()
    }
}

impl ErasedOperation for RefuseUnframedChunkedBody {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let _ = ctx;
            match self.refusal() {
                Some(response) => response,
                // The serializer cannot fail on a fixed code and a fixed sentence; if it ever
                // did, the request keeps the framework's own answer rather than panicking in
                // the request path.
                None => svc.definition().routing_error_response(req),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request head carrying `headers`.
    fn head(headers: &[(&str, &str)]) -> ::http::request::Parts {
        let mut request = ::http::Request::builder().uri("/bucket/key");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.body(()).expect("a request head").into_parts().0
    }

    /// One row of [`only_a_framed_body_without_a_decoded_length_is_refused`]: the case, the
    /// headers the request carries, and whether its body states no payload length.
    type FramingRow<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);

    /// Which bodies state no payload length: the rows differ along whether the encoding
    /// declares the framing (alone or beside another token, in either case), and whether the
    /// decoded length is stated.
    #[test]
    fn only_a_framed_body_without_a_decoded_length_is_refused() {
        let rows: [FramingRow; 6] = [
            ("no encoding at all", &[], false),
            (
                "another encoding entirely",
                &[("content-encoding", "gzip")],
                false,
            ),
            (
                "the framing with its decoded length",
                &[
                    ("content-encoding", "aws-chunked"),
                    ("x-amz-decoded-content-length", "11"),
                ],
                false,
            ),
            (
                "the framing with no decoded length",
                &[("content-encoding", "aws-chunked")],
                true,
            ),
            (
                "the framing beside another encoding, in another case",
                &[("content-encoding", "gzip, AWS-Chunked")],
                true,
            ),
            (
                "a decoded length with no framing",
                &[("x-amz-decoded-content-length", "11")],
                false,
            ),
        ];
        for (case, headers, expected) in rows {
            remember(&head(headers));
            let refused = body_states_no_payload_length();
            assert_eq!(
                refused, expected,
                "case {case:?}: expected the body to state no payload length: {expected}, got \
                 {refused}"
            );
        }
    }

    /// Which algorithm a trailer names: the rows differ along the algorithm, the case the
    /// header is written in, and a value that names no checksum at all.
    #[test]
    fn a_checksum_trailer_names_the_algorithm_its_header_carries() {
        let rows: [(&str, Option<&str>, Option<&str>); 6] = [
            ("no trailer at all", None, None),
            ("a CRC32 trailer", Some("x-amz-checksum-crc32"), Some("CRC32")),
            (
                "a CRC32C trailer",
                Some("x-amz-checksum-crc32c"),
                Some("CRC32C"),
            ),
            (
                "a SHA256 trailer written in upper case",
                Some("X-Amz-Checksum-SHA256"),
                Some("SHA256"),
            ),
            (
                "a trailer that is no checksum header",
                Some("x-amz-meta-note"),
                None,
            ),
            (
                "a checksum header naming no algorithm this service computes",
                Some("x-amz-checksum-nonesuch"),
                None,
            ),
        ];
        for (case, trailer, expected) in rows {
            let headers: Vec<(&str, &str)> = trailer
                .map(|value| vec![(TRAILER_HEADER, value)])
                .unwrap_or_default();
            let named = trailer_checksum_algorithm(&head(&headers));
            assert_eq!(
                named.as_deref(),
                expected,
                "case {case:?}: expected the trailer to name {expected:?}, got {named:?}"
            );
        }
    }
}
