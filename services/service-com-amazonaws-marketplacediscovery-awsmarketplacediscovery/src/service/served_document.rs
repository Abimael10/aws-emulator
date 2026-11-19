// SPDX-License-Identifier: CC0-1.0

//! The ONE way this service answers a fetch of an address it published itself.
//!
//! Several planes of this service hand a client an address on this emulator's own endpoint
//! for it to FETCH later — the buyer-facing pages of a SaaS fulfillment option
//! ([`super::saas_fulfillment_page`]), and the media a catalog record publishes
//! ([`super::catalog_media`]). Each is opened by whatever browser, probe or test holds the
//! address rather than by an SDK, so each is answered as a document rather than in a modelled
//! API envelope — and the rule for that answer is the same wherever it is served: the
//! document's own media type and length, the length reported even for the `HEAD` that asks
//! only what is there, and no sniffing of a document served from this emulator's origin.
//!
//! Stated once here so a change to a header, or to how a `HEAD` is answered, cannot land in
//! one plane and not the other.

use aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};

/// One document this service serves at an address it published: the bytes, their media type
/// and their length.
///
/// `body_wanted` is false for the `HEAD` a client sends when it only wants to know what is
/// there: the answer then carries the document's length and no body.
///
/// `None` only when `status` is not a status code at all, which no caller here passes.
pub(super) fn served_document(
    status: u16,
    media_type: &str,
    document: Vec<u8>,
    body_wanted: bool,
) -> Option<RtResponse> {
    let length = document.len();
    let body = if body_wanted { document } else { Vec::new() };
    let mut response = RtResponse::new(
        StatusCode::try_from(status).ok()?,
        ::aws_smithy_types::body::SdkBody::from(body),
    );
    let headers = response.headers_mut();
    headers.insert("content-type", media_type.to_string());
    // The size of the document itself, which is what a `HEAD` of it reports with no body.
    headers.insert("content-length", length.to_string());
    // Served from this emulator's own origin, so the type it announces is the only type it
    // may be treated as.
    headers.insert("x-content-type-options", "nosniff");
    Some(response)
}
