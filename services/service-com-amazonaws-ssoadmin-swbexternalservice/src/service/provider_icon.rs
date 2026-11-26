// SPDX-License-Identifier: CC0-1.0

//! Serves the PORTAL ICON of an application-provider catalog entry at the address the two
//! provider reads publish for it.
//!
//! `DisplayData$IconUrl` is an absolute URL a client fetches next, and the icon it names is
//! an artifact of the catalog this emulator publishes: so the address the reads hand out
//! names this emulator's own endpoint (`crate::state::provider_icon_url`) and this route
//! answers a fetch of it with that entry's image (`crate::state::provider_icon_image`).
//! Nothing a client is handed leaves the endpoint it is already talking to.
//!
//! IAM Identity Center serves a provider icon ANONYMOUSLY — it is static portal artwork, and
//! the browser rendering a tile carries no SigV4 credential — so the fetch is claimed before
//! the router authenticates anything, through
//! [`smithy_server_runtime::Service::override_routing_error`]: the model binds no operation
//! to the path, because an icon is not an API call but the artifact an API member points at.
//! WHICH icon an address may reach is decided by the catalog alone, so an address naming an
//! entry the catalog does not hold, and one naming an entry that publishes no icon, are
//! answered the same way.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;

/// The methods an icon is fetched on: a `GET` for the image, and a `HEAD` for a client
/// asking only what the asset IS — its media type and its size — which an HTTP asset host
/// answers alike.
const FETCH_METHOD: &str = "GET";
/// See [`FETCH_METHOD`].
const PROBE_METHOD: &str = "HEAD";

/// The media type the one answer of this route that is not an image is served as.
const PLAIN_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// What this route answers an address naming no provider icon of this emulator's: the
/// address is fetched by a plain HTTP client rather than by an AWS SDK, so the answer is the
/// status itself with a one-line reason, and no API error envelope.
const NO_ICON: &str = "no application provider icon at this location\n";

/// Whether `method` and `uri` address a provider icon this route serves — the test
/// `Service::can_handle` claims an anonymous fetch by, since such a request carries no
/// credential scope to resolve a service from.
pub fn claims(method: &str, uri: &str) -> bool {
    (method == FETCH_METHOD || method == PROBE_METHOD)
        && crate::state::provider_icon_route(uri).is_some()
}

/// The answer to a fetch of a provider icon address, or `None` when `req` is not one — in
/// which case the framework's own rendering for an unrouted request stands.
///
/// A pure read of the published catalog: the image is rendered from the catalog entry the
/// path names and nothing about the service changes.
pub fn served(req: &RtRequest) -> Option<RtResponse> {
    let body_wanted = match req.method() {
        FETCH_METHOD => true,
        PROBE_METHOD => false,
        _ => return None,
    };
    let provider_id = crate::state::provider_icon_route(req.uri())?;
    // Nothing is invented for an entry this catalog publishes no icon for: there is nothing
    // at that location, which is also what an id the catalog does not hold answers.
    let Some(image) = crate::state::provider_icon_image(provider_id) else {
        return Some(answer(
            404,
            PLAIN_MEDIA_TYPE,
            NO_ICON.as_bytes().to_vec(),
            true,
        ));
    };
    Some(answer(
        200,
        crate::state::PROVIDER_ICON_MEDIA_TYPE,
        image,
        body_wanted,
    ))
}

/// One answer of this route: the bytes, their media type and the size of the asset itself —
/// which a `HEAD` reports with no body, as HTTP defines it.
fn answer(status: u16, media_type: &str, body: Vec<u8>, body_wanted: bool) -> RtResponse {
    let length = body.len();
    let sent = if body_wanted { body } else { Vec::new() };
    let mut response = RtResponse::new(icon_status(status), SdkBody::from(sent));
    let headers = response.headers_mut();
    headers.insert("content-type", media_type.to_string());
    headers.insert("content-length", length.to_string());
    // The image is served from this emulator's own origin, so the type this response
    // announces is the only type a browser may treat it as.
    headers.insert("x-content-type-options", "nosniff");
    response
}

/// A status this route answers with. Every call site passes a constant, so the conversion
/// cannot fail.
fn icon_status(code: u16) -> StatusCode {
    StatusCode::try_from(code).expect("a provider icon fetch answers constant statuses")
}
