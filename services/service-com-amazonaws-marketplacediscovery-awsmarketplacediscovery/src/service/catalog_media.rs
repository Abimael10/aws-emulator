// SPDX-License-Identifier: CC0-1.0

//! The ROUTES that answer at the addresses a CATALOG RECORD publishes: the logo thumbnail
//! image of a listing or product, and the reviews page a review source keeps for a listing.
//!
//! `logoThumbnailUrl` is the image a client that renders a search result tile fetches, and
//! `ReviewSourceSummary$sourceUrl` is the page it turns into a "read the reviews" link. This
//! emulator publishes both on its own endpoint ([`crate::state::logo_url`],
//! [`crate::state::reviews_page_url`]); this module is the other half of those addresses: the
//! claim that says a fetch of one is this service's to answer, and the answer itself — the
//! image or page of the stored record the path names, and a "there is nothing here" answer for
//! an address naming no record this emulator holds media for.
//!
//! Each fetch carries no credential and names no modelled operation: a logo or a reviews page
//! is opened by whatever browser, probe or test holds the address, exactly as AWS serves them
//! off the marketplace's own media host. So the claim is on the PATH this service itself
//! minted, and the answer is an image or a web page rather than a modelled API envelope.

use smithy_server_runtime::http;

use super::served_document::served_document;

/// What this route answers an address naming no record of this catalog: the address is
/// fetched by a plain HTTP client rather than by an AWS SDK, so the answer is the status with
/// a one-line reason and no API error envelope.
const NO_LOGO: &str = "no listing logo at this location\n";

/// The media type the one answer of the logo route that is not an image is served as.
const PLAIN_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// Whether `parts` addresses media a catalog record publishes, for a request that resolved no
/// routing key of its own: a `GET` (or the `HEAD` that asks only what is there) of exactly a
/// path [`crate::state::logo_path`] or [`crate::state::reviews_page_path`] mints.
pub fn claimed(parts: &http::request::Parts) -> bool {
    if parts.method != http::Method::GET && parts.method != http::Method::HEAD {
        return false;
    }
    let uri = parts.uri.to_string();
    crate::state::logo_route(&uri).is_some() || crate::state::reviews_page_route(&uri).is_some()
}

/// The image or page a fetch of `req` serves, or `None` when `req` addresses no media of this
/// service — which leaves the request to the answer it would otherwise get.
///
/// A READ of the catalog, which nothing mutates after seeding, so fetching a published address
/// changes nothing a later caller sees: the image is the record's own logo and the page
/// restates the review summary `SearchListings` and `GetListing` report.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body_wanted = match req.method() {
        "GET" => true,
        "HEAD" => false,
        _ => return None,
    };
    if let Some(media_id) = crate::state::logo_route(req.uri()) {
        let state = crate::state::service_state(engine)?;
        let image = state.lock().listing_logo_image(media_id);
        // The image of a record this emulator published that address for, and nothing at all
        // for every other address of this shape: no image is invented for a media id the
        // catalog does not hold.
        return match image {
            Some(image) => served_document(
                200,
                crate::state::LOGO_MEDIA_TYPE,
                image,
                body_wanted,
            ),
            None => served_document(
                404,
                PLAIN_MEDIA_TYPE,
                NO_LOGO.as_bytes().to_vec(),
                body_wanted,
            ),
        };
    }
    let reviews_list_id = crate::state::reviews_page_route(req.uri())?;
    let state = crate::state::service_state(engine)?;
    let document = state.lock().listing_reviews_page(reviews_list_id);
    let status = if document.is_some() { 200_u16 } else { 404 };
    let document = document.unwrap_or_else(crate::state::reviews_page_not_found_page);
    served_document(
        status,
        crate::state::REVIEWS_PAGE_MEDIA_TYPE,
        document.into_bytes(),
        body_wanted,
    )
}
