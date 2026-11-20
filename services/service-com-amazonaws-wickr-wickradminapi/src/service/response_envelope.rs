// SPDX-License-Identifier: CC0-1.0

//! The envelope every response of this service carries: the media type it announces, and
//! the entity tag that validates the body it answered with.
//!
//! # What the wire shows
//!
//! The model declares `aws.protocols#restJson1`, which implies `application/json`, and the
//! framework's renderers stamp exactly that — the restJson1 output and error serializers,
//! and the pre-dispatch envelope the refusal tail renders in. The recorded wire shows
//! `application/json; charset=utf-8` on EVERY response of this service, success and error
//! alike, plus an `etag` validating the body. Neither is in the model, and neither is a
//! fact of any one operation: both are properties of the channel every operation answers
//! through, which is why they are applied here once instead of restated per handler.
//!
//! The REQUEST side is untouched: the media type a caller sends is not re-read, and routing
//! stays the model's throughout.
//!
//! # Where each half lives
//!
//! The media-type half is not this service's rule and is not restated here: this module
//! states only the pair its responses are re-announced across ([`RESPONSE_MEDIA_TYPE`]),
//! and the mechanism — the one function that re-announces a response, and the
//! operation-seam wrapper that carries it around the centralized walk — is
//! [`service_common::response_media_type`].
//!
//! The entity-tag half is this module's: a validator derived from the response BYTES, so
//! the same body always yields the same tag and any different body a different one, which
//! is the whole contract of an entity tag.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use service_common::response_media_type::{ResponseMediaType, ResponseMediaTypeOperation};
use sha2::{Digest, Sha256};
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The media type the model's `restJson1` protocol trait implies, which the framework's
/// renderers stamp and which is the ONLY value re-announced.
const PROTOCOL_IMPLIED_MEDIA_TYPE: &str = "application/json";

/// The media type this service announces: the JSON media type WITH its charset parameter,
/// which is what a client or proxy that parses the parameter reads.
const ANNOUNCED_MEDIA_TYPE: &str = "application/json; charset=utf-8";

/// The pair every response of this service is re-announced across.
pub const RESPONSE_MEDIA_TYPE: ResponseMediaType =
    ResponseMediaType::new(PROTOCOL_IMPLIED_MEDIA_TYPE, ANNOUNCED_MEDIA_TYPE);

/// The response header carrying the entity tag.
const ETAG: &str = "etag";

/// How many bytes of the body digest the tag carries: 128 bits, so two distinct bodies
/// cannot be given one tag.
const TAG_DIGEST_BYTES: usize = 16;

/// `resp` in this service's envelope: the announced media type, and an entity tag over the
/// body it carries.
pub fn enveloped(resp: RtResponse) -> RtResponse {
    with_entity_tag(RESPONSE_MEDIA_TYPE.announce(resp))
}

/// `resp` with its entity tag, when it carries a body to derive one from and does not
/// already carry a tag of its own.
///
/// Derived from the serialized body, so it is set only when those bytes are in memory —
/// which they are for everything this service renders, the codec having just produced
/// them. A header the HTTP layer rejected is OMITTED rather than failing the response: on
/// the error path, replacing a modelled failure with a transport failure would lose the
/// error the caller has to read.
fn with_entity_tag(mut resp: RtResponse) -> RtResponse {
    if resp.headers().get(ETAG).is_some() {
        return resp;
    }
    if let Some(body) = resp.body().bytes() {
        let tag = entity_tag(body);
        let _ = resp.headers_mut().try_insert(ETAG, tag);
    }
    resp
}

/// The entity tag for `body`: a quoted, truncated SHA-256 of those bytes.
///
/// A STRONG validator, because it is a function of the response bytes alone and of nothing
/// else — one body is always tagged the same and any different body differently, which is
/// what a client's conditional-request and caching logic keys off. The recorded tag's own
/// value is a per-response value and is not reproduced; what the recording fixes is that
/// the header is there and carries an entity tag.
pub fn entity_tag(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    // A SHA-256 digest is 32 bytes, so the prefix is always there; taken without indexing
    // so this cannot panic whatever the digest length becomes.
    let truncated = digest
        .get(..TAG_DIGEST_BYTES)
        .unwrap_or_else(|| digest.as_slice());
    format!("\"{}\"", hex::encode(truncated))
}

/// Serves the modelled operation this service's routing table names for the request, and
/// answers it in this service's envelope.
///
/// It owns no operation and no routing of its own: the media-type half delegates to the
/// shared [`ResponseMediaTypeOperation`], which runs the very routing-table entry the
/// framework would have run and re-announces what it answered — so the whole deserialize →
/// validate → handle → serialize walk, and every status, code, message and body byte it
/// answers with, stay the framework's. This wrapper adds one header.
///
/// Taking the OPERATION seam is what reaches the response a codec override cannot: the
/// framework-level refusal tail, which renders a constraint the model declares in the
/// model-derived envelope INSIDE the centralized walk.
#[derive(Debug, Clone, Copy)]
pub struct ResponseEnvelopeOperation {
    media_type: ResponseMediaTypeOperation,
}

impl ResponseEnvelopeOperation {
    /// The wrapper this service serves every routed request through. A service builds one
    /// in its constructor, because `Service::override_operation` hands back a borrow of it.
    pub const fn new() -> Self {
        Self {
            media_type: ResponseMediaTypeOperation::new(RESPONSE_MEDIA_TYPE),
        }
    }
}

impl Default for ResponseEnvelopeOperation {
    fn default() -> Self {
        Self::new()
    }
}

impl ErasedOperation for ResponseEnvelopeOperation {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { with_entity_tag(self.media_type.run(svc, req, ctx).await) })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names" — which is the
        // operation this wrapper delegates to, so its model-declared schemes decide and
        // this wrapper cannot weaken them.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::body::SdkBody;

    /// The response header carrying the media type.
    const CONTENT_TYPE: &str = "content-type";

    /// A response carrying `body`, and `content_type` when there is one.
    fn response(content_type: Option<&str>, body: &str) -> RtResponse {
        let mut resp = RtResponse::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(200).expect("200 is a status"),
            SdkBody::from(body.as_bytes().to_vec()),
        );
        if let Some(content_type) = content_type {
            resp.headers_mut()
                .insert(CONTENT_TYPE, content_type.to_string());
        }
        resp
    }

    /// The protocol-implied media type becomes the announced one; every other value, and a
    /// response with no media type at all, is left as its producer wrote it.
    ///
    /// The rows span what a producer can put in the header: the implied value (what the
    /// framework's renderers stamp), the announced value (already right), a deliberately
    /// different media type, and no header.
    #[test]
    fn only_the_protocol_implied_media_type_is_reannounced() {
        let cases: [(Option<&str>, Option<&str>); 4] = [
            (Some(PROTOCOL_IMPLIED_MEDIA_TYPE), Some(ANNOUNCED_MEDIA_TYPE)),
            (Some(ANNOUNCED_MEDIA_TYPE), Some(ANNOUNCED_MEDIA_TYPE)),
            (Some("text/plain"), Some("text/plain")),
            (None, None),
        ];

        for (given, expected) in cases {
            let answered = enveloped(response(given, "{}"));
            let actual = answered.headers().get(CONTENT_TYPE).map(str::to_string);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {given:?}: expected content-type {expected:?}, got {actual:?}"
            );
        }
    }

    /// The entity tag is a function of the body alone: one body always tags the same, and
    /// any two distinct bodies tag differently.
    ///
    /// The rows span the bodies this service's responses carry: an empty body, a success
    /// payload, the same payload with one character changed, two payloads differing only
    /// in member order, and an error envelope.
    #[test]
    fn the_entity_tag_is_a_function_of_the_body() {
        let bodies = [
            "",
            r#"{"networkId":"10000000"}"#,
            r#"{"networkId":"10000001"}"#,
            r#"{"message":"x","networkId":"10000000"}"#,
            r#"{"networkId":"10000000","message":"x"}"#,
            r#"{"__type":"ResourceNotFoundError","message":"x"}"#,
        ];

        for body in bodies {
            let (first, again) = (entity_tag(body.as_bytes()), entity_tag(body.as_bytes()));
            assert_eq!(
                first, again,
                "case {body:?}: one body must always tag the same, got {first} and {again}"
            );
        }
        for (index, body) in bodies.iter().enumerate() {
            for other in &bodies[index + 1..] {
                let (tag, other_tag) = (entity_tag(body.as_bytes()), entity_tag(other.as_bytes()));
                assert_ne!(
                    tag, other_tag,
                    "case ({body:?}, {other:?}): distinct bodies must tag differently, both \
                     got {tag}"
                );
            }
        }
    }

    /// A tag a producer already set is kept rather than overwritten.
    #[test]
    fn an_existing_tag_is_kept() {
        let mut given = response(Some(PROTOCOL_IMPLIED_MEDIA_TYPE), "{}");
        given.headers_mut().insert(ETAG, "\"chosen\"");
        let answered = enveloped(given);
        let actual = answered.headers().get(ETAG).map(str::to_string);
        assert_eq!(
            actual.as_deref(),
            Some("\"chosen\""),
            "case \"a producer's own tag\": expected it kept, got {actual:?}"
        );
    }
}
