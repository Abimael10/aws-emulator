// SPDX-License-Identifier: CC0-1.0

//! The media type a response whose BODY IS A TEXT REPORT announces.
//!
//! Two of this plane's operations answer with a report rather than with a JSON document:
//! their output binds one member as the response's whole `@httpPayload` and the model types
//! that member as the text-report shape, declaring the media type of its bytes —
//! `smithy_query(query="operation", operation="ExecuteGremlinExplainQuery,ExecuteGremlinProfileQuery")`
//! reports `output` targeting `ReportAsText` with `httpPayload: true` and
//! `mediaType: "text/plain"`.
//!
//! Smithy maps a BLOB payload to [`PAYLOAD_IMPLIED_MEDIA_TYPE`] — the media type of an
//! opaque download — and that is what the framework's serializer stamps, because it reads
//! the payload's type and not the media type the model declares for it. A client told the
//! bytes are an opaque download saves them instead of reading them, so the response
//! announces the media type the MODEL declares for that payload
//! ([`TEXT_REPORT_MEDIA_TYPE`]).
//!
//! Only that one header value changes, and only on a response the renderer stamped the
//! payload-implied type on: a refusal of one of these operations is a JSON body like any
//! other and keeps the media type this service announces for JSON
//! ([`crate::service::RESPONSE_MEDIA_TYPE`]), which the wrapper below reaches by serving the
//! whole request walk through it first. The payload of an operation whose model declares no
//! media type for it (this plane's openCypher explain binds a plain `Blob`) is left
//! announcing the opaque type Smithy implies.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::response_media_type::{ResponseMediaType, ResponseMediaTypeOperation};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The media type the `restJson1` renderer stamps on a response whose body is a blob
/// `@httpPayload` — Smithy's default for a blob, and the only value this module replaces.
const PAYLOAD_IMPLIED_MEDIA_TYPE: &str = "application/octet-stream";

/// The media type the model declares for the text report these operations answer with:
/// `@mediaType "text/plain"` on the payload shape. A payload the model declares a media
/// type for is announced AS that media type — the report is text a client reads, not an
/// opaque download — and the model is what states it, so nothing else is read for it.
const TEXT_REPORT_MEDIA_TYPE: &str = "text/plain";

/// The pair a text-report payload is re-announced across.
const MEDIA_TYPE: ResponseMediaType =
    ResponseMediaType::new(PAYLOAD_IMPLIED_MEDIA_TYPE, TEXT_REPORT_MEDIA_TYPE);

/// The operations whose payload the model declares a media type for, by the name the
/// protocol resolves as their routing key.
///
/// Source: the model's two operations binding `ReportAsText` as their `@httpPayload`
/// (`smithy_query(query="operation", operation="ExecuteGremlinExplainQuery,ExecuteGremlinProfileQuery")`).
pub const TEXT_REPORT_OPERATIONS: &[&str] =
    &["ExecuteGremlinExplainQuery", "ExecuteGremlinProfileQuery"];

/// One operation whose text-report payload announces the media type the model declares for
/// it, and whose every other response announces what this service announces for JSON.
pub struct TextReportResponse {
    /// The service-wide JSON announcement. It serves the modelled operation the routing key
    /// names — the refusal the framework renders inside that walk included — and announces
    /// the JSON media type on what it answered.
    announced: ResponseMediaTypeOperation,
}

impl TextReportResponse {
    /// Serve the walk through `announced`, then announce the payload's own media type.
    pub const fn new(announced: ResponseMediaTypeOperation) -> Self {
        Self { announced }
    }
}

impl ErasedOperation for TextReportResponse {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let response = self.announced.run(svc, req, ctx).await;
            // The status, the body and every other header are the renderer's; only the
            // media type the payload declares is replaced, and only where the renderer
            // stamped the payload-implied one.
            MEDIA_TYPE.announce(response)
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // Not an operation of its own: the request is signed as the MODEL says for the
        // operation this wrapper stands in front of, so this wrapper cannot weaken it.
        self.announced.auth_schemes()
    }
}
