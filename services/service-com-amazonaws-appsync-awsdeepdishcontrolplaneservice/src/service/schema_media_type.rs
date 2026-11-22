// SPDX-License-Identifier: CC0-1.0

//! The media type the SCHEMA an introspection read answers with is labelled.
//!
//! `GetIntrospectionSchema` answers with the schema document itself: its output binds one
//! member as the response's whole `@httpPayload`, and the model types that member as an
//! opaque `Blob` (`smithy_query(query="operation",operation="GetIntrospectionSchema")`).
//! Smithy maps a blob payload to `application/octet-stream`, which is what the framework's
//! serializer labels the response with — the media type of an opaque download.
//!
//! RECORDED behaviour: AppSync labels that payload `application/json`, in both the
//! representations the call's `format` can ask for. The bytes are a text document the caller
//! reads — GraphQL SDL, or a GraphQL introspection result — rather than an opaque download,
//! so a browser or an HTTP client is told to display or parse it rather than to save it.
//!
//! The re-announcement itself is the shared rule of [`::service_common::response_media_type`]
//! — this service states only its own pair, [`PAYLOAD_MEDIA_TYPE`] ->
//! [`ANNOUNCED_MEDIA_TYPE`], so every other response (every JSON document this `restJson1`
//! service's renderer already stamps, a modelled refusal among them) is left exactly as its
//! producer wrote it.
//!
//! Where this service takes it: in a layer in FRONT of the operation rather than in the
//! handler, because the handler answers the modelled output and which media type the wire
//! carries it under is a property of the response the protocol built from it. The layer wraps
//! the gate IAM's decision is taken in ([`super::authorization::Gated`]) rather than
//! resolving the modelled operation itself, so the request still walks the one path every
//! other operation of this service walks and nothing of the decision is bypassed.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::response_media_type::ResponseMediaType;
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The media type the `restJson1` renderer stamps on a response whose body is a blob
/// `@httpPayload` — Smithy's default for a blob, and the only value this service
/// re-announces.
const PAYLOAD_MEDIA_TYPE: &str = "application/octet-stream";

/// The media type AppSync labels an introspection schema with.
const ANNOUNCED_MEDIA_TYPE: &str = "application/json";

/// The pair this service's schema payload is re-announced across.
const MEDIA_TYPE: ResponseMediaType =
    ResponseMediaType::new(PAYLOAD_MEDIA_TYPE, ANNOUNCED_MEDIA_TYPE);

/// One operation whose payload is announced as the media type AppSync labels a schema with.
pub struct AnnouncedAsJson<Inner> {
    inner: Inner,
}

impl<Inner> AnnouncedAsJson<Inner> {
    /// Serve `inner`, announcing the media type AppSync labels its payload with.
    pub fn new(inner: Inner) -> Self {
        Self { inner }
    }
}

impl<Inner: ErasedOperation> ErasedOperation for AnnouncedAsJson<Inner> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let response = self.inner.run(svc, req, ctx).await;
            // The status, the body and every other header are the renderer's; only the media
            // type the payload declares is this service's own, and only where the renderer
            // stamped the payload-implied one.
            MEDIA_TYPE.announce(response)
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The layer is not an operation of its own: the request is signed as the MODEL says
        // for the operation it names, so authentication is decided by the operation this
        // layer stands in front of.
        self.inner.auth_schemes()
    }
}
