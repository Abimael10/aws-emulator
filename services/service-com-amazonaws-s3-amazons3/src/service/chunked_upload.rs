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
//!
//! And because the two say the same thing, they are JUDGED the same way: a checksum value a
//! write declares has to describe the bytes the request carried, wherever the request declares
//! it. A trailing value that does not is the `BadDigest` every declared checksum of this
//! service that does not describe its bytes earns ([`crate::state::verify_declared_checksum`]
//! holds that one comparison), and a trailer chunk carrying a header OTHER than the one
//! `x-amz-trailer` declared is trailing data that does not conform to the framing at all,
//! which is [`MALFORMED_TRAILER_CODE`]. Either way the request is refused before any handler
//! runs, so nothing is stored — which is the whole point of a trailing checksum: it is the
//! only corruption signal a streamed upload has.
//!
//! The trailer is read where the framed body still exists: the core strips the framing off
//! before any operation is reached and skips the trailing headers without interpreting them,
//! so the one seam this service is handed the request with its body AS IT ARRIVED is the
//! protocol decision ([`super::EmulatedService::wire_protocol`], which the framework takes
//! before it decodes the body). The verdict is kept as a request-scoped note, like the framing
//! of the head above, and the refusal it earns is routed in place of the operation the address
//! names ([`super::EmulatedService::override_operation`]).

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
    // Whatever verdict this thread may still hold about an earlier request's trailer is
    // dropped here, so a refusal can only ever be this request's: the body it is read from is
    // handed to this service later in the same dispatch
    // ([`remember_framed_body`]), and a request that sends no trailer at all leaves no
    // verdict behind.
    TRAILER_VERDICT.with(|verdict| *verdict.borrow_mut() = None);
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

/// The routing key the TRAILER refusal below is held under — a key of the same kind as
/// [`REFUSE_UNFRAMED_CHUNKED_BODY`], which no modelled request can reach by name.
pub const REFUSE_MALFORMED_TRAILER: &str = "@refuse-malformed-trailer";

/// The code S3 answers a framed body whose trailing data does not conform to the framing it
/// declared: `MalformedTrailerError` (Amazon S3 API Reference, *Error responses*), HTTP 400.
pub const MALFORMED_TRAILER_CODE: &str = "MalformedTrailerError";
/// See [`MALFORMED_TRAILER_CODE`] — the FIXED sentence that code carries, which states the
/// class of fault and nothing of the trailer the request sent.
pub const MALFORMED_TRAILER_MESSAGE: &str = "The request contained trailing data that was not \
     well-formed or did not conform to our published schema.";
/// The HTTP status [`MALFORMED_TRAILER_CODE`] is rendered with: 400, the status of every
/// malformed-request code in that list.
pub const MALFORMED_TRAILER_STATUS: u16 = 400;

/// Why the TRAILER of the framed body this thread is serving cannot be accepted.
#[derive(Debug, Clone)]
pub(super) enum TrailerRefusal {
    /// The trailing headers are not the ones `x-amz-trailer` declared: a name the request did
    /// not declare arrived, or a name it declared did not.
    Malformed,
    /// The trailing checksum is not a digest of the payload received, judged by the one rule
    /// every declared checksum of this service is judged by
    /// ([`crate::state::verify_declared_checksum`]).
    Integrity(crate::state::IntegrityRejection),
}

impl TrailerRefusal {
    /// The wire code this refusal carries.
    fn code(&self) -> &'static str {
        match self {
            Self::Malformed => MALFORMED_TRAILER_CODE,
            Self::Integrity(rejection) => rejection.code(),
        }
    }

    /// The HTTP status it is rendered with.
    fn status(&self) -> u16 {
        match self {
            Self::Malformed => MALFORMED_TRAILER_STATUS,
            Self::Integrity(rejection) => rejection.status(),
        }
    }

    /// The `<Message>` it carries: the fixed sentence for malformed trailing data, and for a
    /// checksum the sentence that rejection publishes — which names the algorithm the
    /// request's own `x-amz-trailer` declared.
    fn message(&self) -> String {
        match self {
            Self::Malformed => MALFORMED_TRAILER_MESSAGE.to_string(),
            Self::Integrity(rejection) => rejection.to_string(),
        }
    }
}

thread_local! {
    /// Why the trailer of the request this thread is serving cannot be accepted, or `None`
    /// for a request whose trailer is the one it declared and describes its payload — which
    /// is every request that sends no trailer at all.
    ///
    /// A request-scoped note under the same discipline as [`FRAMING`]: it is cleared for
    /// every request this service is offered ([`remember`]) and recorded, for a framed body,
    /// where that body is still framed ([`remember_framed_body`]).
    static TRAILER_VERDICT: RefCell<Option<TrailerRefusal>> = const { RefCell::new(None) };
}

/// Judge the TRAILER of `request`'s body, as the request this thread is serving.
///
/// A no-op for every request whose body is not framed — the overwhelmingly common case, and a
/// pure header check — and for a framed body that declares no trailer: there is then nothing
/// the request said about trailing data to hold it to.
///
/// Called where the body is still FRAMED (see this module's header): the core decodes the
/// framing away before any operation is reached and skips the trailing headers without
/// interpreting them, so this is the only place the declared trailer and the trailer actually
/// sent can be compared.
pub(super) fn remember_framed_body(request: &RtRequest) {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let framed = header(CONTENT_ENCODING_HEADER).is_some_and(|encoding| {
        encoding
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case(AWS_CHUNKED))
    });
    let Some(declared) = header(TRAILER_HEADER).filter(|_| framed) else {
        return;
    };
    let declared: Vec<String> = declared
        .split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    let Some(body) = request.body().bytes() else {
        return;
    };
    let verdict = judged_trailer(&declared, body);
    TRAILER_VERDICT.with(|slot| *slot.borrow_mut() = verdict);
}

/// Why the trailer of the request this thread is serving cannot be accepted, or `None`.
pub(super) fn trailer_refusal() -> Option<TrailerRefusal> {
    TRAILER_VERDICT.with(|slot| slot.borrow().clone())
}

/// The refusal the trailing data of `framed` earns, given the trailer names the request
/// `declared`, or `None` when the trailing headers are the declared ones and the checksum among
/// them describes the payload.
///
/// A body whose FRAMING does not parse earns nothing here: such a body is refused for the
/// framing itself, by the decode the framework performs next, and a verdict about the trailing
/// data of bytes that do not frame a payload would say nothing true.
///
/// The work this does is bounded by the body alone: the trailing headers are read up to
/// [`MAX_TRAILING_HEADERS`] and the payload is digested AT MOST ONCE, under the one algorithm
/// a write may declare a checksum for — "you can only specify one checksum algorithm per
/// request" (`PutObjectRequest$ChecksumAlgorithm`) — so no count the caller chooses multiplies
/// the pass over the payload.
fn judged_trailer(declared: &[String], framed: &[u8]) -> Option<TrailerRefusal> {
    let section = trailing_headers(framed)?;
    // A section whose lines are not `name:value` at all is trailing data that does not conform
    // to the framing either, so it is refused rather than read as carrying nothing.
    let Some(sent) = trailer_entries(section) else {
        return Some(TrailerRefusal::Malformed);
    };
    // More trailing headers than the framing has any use for is trailing data that does not
    // conform to it, and is refused before anything is read out of them.
    if sent.len() > MAX_TRAILING_HEADERS {
        return Some(TrailerRefusal::Malformed);
    }
    // The names have to be the ones the request declared, in either direction: "if you specify
    // `x-amz-trailer`, the checksum is sent in the trailing header" names WHICH header the
    // trailing data carries, so a name the request did not declare — and a declared name that
    // never arrived — is trailing data that does not conform to the framing it announced. The
    // one exception is the line the FRAMING itself appends: a signed streaming payload carries
    // its chain's final signature as a trailing header ([`TRAILER_SIGNATURE_HEADER`]), which
    // the signing mode defines and `x-amz-trailer` never declares.
    let declared_as_sent = declared
        .iter()
        .all(|name| sent.iter().any(|(sent_name, _)| sent_name == name));
    let sent_as_declared = sent.iter().all(|(name, _)| {
        name == TRAILER_SIGNATURE_HEADER || declared.iter().any(|declared_name| declared_name == name)
    });
    if !declared_as_sent || !sent_as_declared {
        return Some(TrailerRefusal::Malformed);
    }
    // And the checksum among them has to describe the PAYLOAD those chunks decode to, which is
    // the object the write would store: the same comparison a checksum declared up front in a
    // header is judged by ([`crate::state::verify_declared_checksum`]).
    let (name, value) = sent
        .iter()
        .find(|(name, _)| checksum_trailer_algorithm(name).is_some())?;
    let algorithm = checksum_trailer_algorithm(name)?;
    let payload = ::aws_smithy_schema_server::aws_chunked::decode(framed, None).ok()?;
    crate::state::verify_declared_checksum(&algorithm, value, &payload)
        .err()
        .map(TrailerRefusal::Integrity)
}

/// The trailing header a SIGNED streaming payload's framing appends after the trailers the
/// request declared: the final signature of the chunk chain, which SigV4 defines and
/// `x-amz-trailer` never names.
const TRAILER_SIGNATURE_HEADER: &str = "x-amz-trailer-signature";

/// How many trailing headers are read out of one body at most — the trailers a request may
/// declare plus the signature line the framing appends, with room to spare.
///
/// A cap rather than an interpretation: the count comes from the caller's own bytes, so it is
/// what bounds the reading below (and the work it does) rather than the caller.
const MAX_TRAILING_HEADERS: usize = 8;


/// The bytes of `framed` that follow its terminating zero-length chunk — the trailing headers
/// — or `None` for a body whose chunk framing does not parse.
///
/// The walk is the framing's own: each chunk is a hexadecimal size, optional chunk extensions
/// after a `;`, CRLF, that many bytes and CRLF again, and the zero-sized chunk ends the
/// payload. Every step is bounded BY THE BODY — a declared size that would carry `position`
/// past the bytes received ends the walk, which is also the only size a chunk can have — and
/// free of recursion, so no size a request declares can run the walk off the end or past the
/// width of an index.
fn trailing_headers(framed: &[u8]) -> Option<&[u8]> {
    let mut position = 0usize;
    loop {
        let line = framed.get(position..)?;
        let line_end = position + line.windows(CRLF.len()).position(|pair| pair == CRLF)?;
        let header = framed.get(position..line_end)?;
        let size_field = header.split(|byte| *byte == b';').next().unwrap_or(header);
        let size_text = ::std::str::from_utf8(size_field).ok()?;
        let size = usize::from_str_radix(size_text.trim(), 16).ok()?;
        position = within(line_end, CRLF.len(), framed)?;
        if size == 0 {
            return framed.get(position..);
        }
        position = within(position, size, framed)?;
        if framed.get(position..within(position, CRLF.len(), framed)?) != Some(CRLF) {
            return None;
        }
        position = within(position, CRLF.len(), framed)?;
    }
}

/// `position + step`, when that lands inside `framed`, and `None` when it does not — which is
/// every advance no body of these bytes could make, including one wide enough to run past the
/// width of an index.
fn within(position: usize, step: usize, framed: &[u8]) -> Option<usize> {
    position
        .checked_add(step)
        .filter(|advanced| *advanced <= framed.len())
}

/// The trailing headers `section` carries, each as its lower-cased name and its value: one
/// `name:value` per CRLF-terminated line, which is how the framing writes them — and `None`
/// for a section that is not that at all: bytes that are not UTF-8, or a non-empty line
/// carrying no `:`.
///
/// At most [`MAX_TRAILING_HEADERS`] + 1 of them are read, which is what bounds this reading
/// against a section of nothing but trailing headers: one more than the cap is already enough
/// for the caller of this to refuse the section ([`judged_trailer`]).
fn trailer_entries(section: &[u8]) -> Option<Vec<(String, String)>> {
    let text = ::std::str::from_utf8(section).ok()?;
    text.split("\r\n")
        .filter(|line| !line.trim().is_empty())
        .take(MAX_TRAILING_HEADERS + 1)
        .map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect()
}

/// The `ChecksumAlgorithm` value the trailing header `name` carries a digest under, or `None`
/// for a trailing header that is no checksum of this service's at all.
///
/// The same reading [`trailer_checksum_algorithm`] makes of the DECLARED name, over a name as
/// the trailing data spelled it, so what is judged and what the object is stored under cannot
/// disagree about which algorithm a trailer names.
fn checksum_trailer_algorithm(name: &str) -> Option<String> {
    let algorithm = name
        .trim()
        .to_ascii_lowercase()
        .strip_prefix(CHECKSUM_TRAILER_PREFIX)?
        .to_ascii_uppercase();
    crate::ops::types::CHECKSUM_ALGORITHM_VALUES
        .contains(&algorithm.as_str())
        .then_some(algorithm)
}

/// The two bytes every line of the framing is terminated by.
const CRLF: &[u8] = b"\r\n";

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

/// The operation that answers the TRAILER refusal: the code, status and sentence the verdict
/// this request's own trailer earned carries ([`TrailerRefusal`]), rendered in this service's
/// envelope exactly as the refusal above is.
pub struct RefuseMalformedTrailer {
    /// This service's own protocol: the envelope every refusal of this service goes out in.
    protocol: UnwrappedXmlRestXml,
}

impl RefuseMalformedTrailer {
    /// Build the refusal operation. INFALLIBLE, and it holds no state: WHAT it answers is the
    /// request-scoped verdict the trailer of the request being served earned.
    pub fn new() -> Self {
        Self {
            protocol: UnwrappedXmlRestXml::new(),
        }
    }

    /// The envelope for `refusal`.
    fn refusal(&self, refusal: &TrailerRefusal) -> Option<RtResponse> {
        let rendered = crate::scaffolded::Unmodeled {
            code: refusal.code(),
            status: refusal.status(),
            message: refusal.message(),
        };
        self.protocol
            .serialize_error(
                &rendered,
                &crate::scaffolded::UNMODELED_SCHEMA,
                refusal.code(),
                refusal.status(),
            )
            .ok()
    }
}

impl Default for RefuseMalformedTrailer {
    fn default() -> Self {
        Self::new()
    }
}

impl ErasedOperation for RefuseMalformedTrailer {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let _ = ctx;
            let rendered = trailer_refusal().and_then(|refusal| self.refusal(&refusal));
            match rendered {
                Some(response) => response,
                // Reached only if the verdict is gone or the serializer failed on a published
                // code and a published sentence; the request keeps the framework's own answer
                // rather than panicking in the request path.
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

    /// The Base64 CRC32 of `payload`, computed by the SDK's own checksum crate.
    fn crc32_of(payload: &[u8]) -> String {
        let parsed: ::aws_smithy_checksums::ChecksumAlgorithm =
            "crc32".parse().expect("the SDK computes CRC32");
        let mut checksum = parsed.into_impl();
        checksum.update(payload);
        ::aws_smithy_types::base64::encode(checksum.finalize())
    }

    /// `payload` framed in one chunk, terminated, with the trailing header `name: value`.
    fn framed_body(payload: &[u8], name: &str, value: &str) -> Vec<u8> {
        let mut body = format!("{:x}\r\n", payload.len()).into_bytes();
        body.extend_from_slice(payload);
        body.extend_from_slice(b"\r\n0\r\n");
        body.extend_from_slice(format!("{name}:{value}\r\n\r\n").as_bytes());
        body
    }

    /// RULE: the trailing data of a framed body is read where the framing says it begins, and a
    /// body whose framing does not parse yields none — so nothing of a malformed, truncated or
    /// oversized body is read as a trailer.
    ///
    /// The rows feed the reading what an untrusted client can send: the well-formed body, one
    /// whose declared chunk size is longer than the bytes that follow, one whose size field is
    /// not hexadecimal at all, one whose chunk header is never terminated, an empty body, a
    /// body whose single chunk declares a size no machine could hold, and one whose declared
    /// size is the widest an index can carry — the value that would run an unchecked advance
    /// past that width.
    #[test]
    fn only_a_body_whose_framing_parses_carries_trailing_headers() {
        let payload = b"hello-world-exact-bytes";
        let well_formed = framed_body(payload, "x-amz-checksum-crc32", &crc32_of(payload));
        let rows: [(&str, Vec<u8>, bool); 8] = [
            ("a well-formed framed body", well_formed.clone(), true),
            (
                "a chunk declaring more bytes than follow it",
                b"20\r\nshort\r\n0\r\n\r\n".to_vec(),
                false,
            ),
            (
                "a size field that is not hexadecimal",
                b"zz\r\npayload\r\n0\r\n\r\n".to_vec(),
                false,
            ),
            (
                "a chunk header that is never terminated",
                b"17".to_vec(),
                false,
            ),
            ("no body at all", Vec::new(), false),
            (
                "a chunk whose declared size no machine could hold",
                b"ffffffffffffffffffffffff\r\n".to_vec(),
                false,
            ),
            (
                "a chunk whose declared size is the widest an index carries",
                b"ffffffffffffffff\r\n".to_vec(),
                false,
            ),
            (
                "a chunk whose declared size lands one index short of that width",
                b"ffffffffffffffed\r\n".to_vec(),
                false,
            ),
        ];
        for (case, body, carries) in rows {
            let section = trailing_headers(&body);
            assert_eq!(
                section.is_some(),
                carries,
                "case {case:?}: expected trailing headers: {carries}, got {section:?}"
            );
        }

        // And the entries of the well-formed one are the name and value it really carried.
        let entries = trailer_entries(
            trailing_headers(&well_formed).expect("the well-formed body frames a payload"),
        )
        .expect("the well-formed body carries one trailing header");
        assert_eq!(
            entries,
            vec![(
                "x-amz-checksum-crc32".to_string(),
                crc32_of(payload).to_string()
            )],
            "expected the trailing header the body carried, got {entries:?}"
        );
    }

    /// RULE: the trailing checksum has to be the digest of the payload the chunks decode to, and
    /// the trailing header has to be the one `x-amz-trailer` declared — beside the signature
    /// line a SIGNED streaming payload's own framing appends, which the request never declares.
    ///
    /// The rows differ along what the trailing data gets wrong: nothing, the value, the name, a
    /// declared name that never arrived, and a section of more trailing headers than the framing
    /// has any use for; plus the signed form, which is served.
    #[test]
    fn the_trailer_is_judged_against_the_payload_and_the_declared_name() {
        let payload = b"hello-world-exact-bytes";
        let declared = vec!["x-amz-checksum-crc32".to_string()];
        let digest = crc32_of(payload);
        // The signed streaming form: the declared trailer, then the final signature of the
        // chunk chain, which SigV4's framing appends and `x-amz-trailer` never names.
        let mut signed = framed_body(payload, "x-amz-checksum-crc32", &digest);
        signed.truncate(signed.len() - 2);
        signed.extend_from_slice(format!("{TRAILER_SIGNATURE_HEADER}:abc123\r\n\r\n").as_bytes());
        // A section of nothing but trailing headers, past the count the framing has a use for.
        let mut crowded = framed_body(payload, "x-amz-checksum-crc32", &digest);
        crowded.truncate(crowded.len() - 2);
        for _ in 0..MAX_TRAILING_HEADERS {
            crowded.extend_from_slice(format!("x-amz-checksum-crc32:{digest}\r\n").as_bytes());
        }
        crowded.extend_from_slice(b"\r\n");
        let rows: [(&str, Vec<u8>, Option<&str>); 7] = [
            (
                "the digest of the payload under the declared name",
                framed_body(payload, "x-amz-checksum-crc32", &digest),
                None,
            ),
            ("the signed form, carrying its signature trailer", signed, None),
            (
                "the digest of other bytes",
                framed_body(payload, "x-amz-checksum-crc32", &crc32_of(b"other bytes")),
                Some(crate::state::BAD_DIGEST_CODE),
            ),
            (
                "a trailing header the request never declared",
                framed_body(payload, "x-amz-checksum-sha256", &digest),
                Some(MALFORMED_TRAILER_CODE),
            ),
            (
                "no trailing header at all",
                b"17\r\nhello-world-exact-bytes\r\n0\r\n\r\n".to_vec(),
                Some(MALFORMED_TRAILER_CODE),
            ),
            (
                "more trailing headers than the framing has a use for",
                crowded,
                Some(MALFORMED_TRAILER_CODE),
            ),
            (
                "a trailing line that is no header at all",
                {
                    let mut body = framed_body(payload, "x-amz-checksum-crc32", &digest);
                    body.truncate(body.len() - 2);
                    body.extend_from_slice(b"not a header line\r\n\r\n");
                    body
                },
                Some(MALFORMED_TRAILER_CODE),
            ),
        ];
        for (case, body, expected) in rows {
            let verdict = judged_trailer(&declared, &body);
            let code = verdict.as_ref().map(TrailerRefusal::code);
            assert_eq!(
                code, expected,
                "case {case:?}: expected the verdict {expected:?}, got {code:?}"
            );
        }
    }
}
