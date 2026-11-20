// SPDX-License-Identifier: CC0-1.0

//! The one response a page this service PUBLISHES is served in.
//!
//! Two addresses of this service are opened by whoever holds the URL rather than called as
//! a modelled operation — a consent portal's page ([`super::consent_portal`]) and a payment
//! connector's authorization address ([`super::payment_connector_authorization`]) — and both
//! answer the same way: the document of the stored resource, or a document saying there is
//! none, as a web page from this emulator's own origin. This module is that shared answer,
//! so a later change to a header or to the not-found status is made once rather than in each
//! plane.

/// The response a fetch of a published page is answered with: `document` as a web page of
/// `media_type`, under `status`, with the body omitted for the `HEAD` that asks only what is
/// there (`body_wanted` false) while still reporting the page's own size.
///
/// `None` only when `status` is not a status code at all, which leaves the request to the
/// answer it would otherwise get.
pub fn served_page(
    status: u16,
    media_type: &str,
    document: &str,
    body_wanted: bool,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body = if body_wanted {
        document.as_bytes().to_vec()
    } else {
        Vec::new()
    };
    let mut response = aws_smithy_runtime_api::http::Response::new(
        aws_smithy_runtime_api::http::StatusCode::try_from(status).ok()?,
        ::aws_smithy_types::body::SdkBody::from(body),
    );
    let headers = response.headers_mut();
    headers.insert("content-type", media_type.to_string());
    // The size of the page itself, which is what a `HEAD` of it reports with no body.
    headers.insert("content-length", document.len().to_string());
    // The page is served from this emulator's own origin, so the type it announces is the
    // only type it may be treated as.
    headers.insert("x-content-type-options", "nosniff");
    Some(response)
}

/// The status a fetch of a published address is answered under: the page of a resource this
/// emulator holds, and a page saying there is none for every other address of that shape —
/// nothing is invented for a resource that was never created, or whose removal has
/// completed.
pub const PAGE_FOUND: u16 = 200;
pub const PAGE_NOT_FOUND: u16 = 404;
