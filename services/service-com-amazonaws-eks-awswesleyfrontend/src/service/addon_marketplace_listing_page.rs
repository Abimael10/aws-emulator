// SPDX-License-Identifier: CC0-1.0

//! The ROUTE that answers at a Marketplace add-on listing's published address.
//!
//! A Marketplace add-on reports `marketplaceInformation.productUrl` for a
//! SUBSCRIBER to follow, and this emulator publishes it on its own endpoint
//! ([`crate::state::addon_marketplace_listing_url`]). This module is the other half
//! of that address: the claim that says a fetch of it is this service's to answer,
//! and the answer itself — the page of the listing the path names
//! ([`crate::state::ComAmazonawsEksAwswesleyfrontendState::addon_marketplace_listing_page`]),
//! and a not-found page for an address naming no listing this catalog holds.
//!
//! The fetch carries no credential and names no modelled operation: a product
//! address is opened by whatever browser, probe or test holds it, exactly as AWS
//! serves the AWS Marketplace's own host. So the claim is on the PATH this service
//! itself minted, and the answer is a web page rather than a modelled API envelope.

use smithy_server_runtime::http;

/// Whether `parts` addresses a listing page this service publishes: a `GET` (or the
/// `HEAD` that asks only what is there) of exactly a path
/// [`crate::state::addon_marketplace_listing_path`] mints.
pub fn claimed(parts: &http::request::Parts) -> bool {
    (parts.method == http::Method::GET || parts.method == http::Method::HEAD)
        && crate::state::addon_marketplace_listing_route(&parts.uri.to_string()).is_some()
}

/// The page a fetch of `req` renders, or `None` when `req` addresses no listing page
/// of this service — which leaves the request to the answer it would otherwise get.
///
/// A READ of the catalog, which nothing writes after seeding, so opening a published
/// address changes nothing a later caller sees: the page restates the listing
/// `DescribeAddonVersions` reports.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body_wanted = match req.method() {
        "GET" => true,
        "HEAD" => false,
        _ => return None,
    };
    let listing_unit = crate::state::addon_marketplace_listing_route(req.uri())?;
    let state = crate::state::service_state(engine)?;
    let document = state.lock().addon_marketplace_listing_page(listing_unit);
    // The page of a listing this emulator published that address for, and a page
    // saying there is none for every other address of this shape: nothing is
    // invented for a unit the catalog sells nothing under.
    let status = if document.is_some() { 200_u16 } else { 404 };
    let document = document.unwrap_or_else(crate::state::addon_marketplace_listing_not_found_page);
    // The size of the page itself, which is what a `HEAD` of it reports with no body.
    let length = document.len();
    let body = if body_wanted {
        document.into_bytes()
    } else {
        Vec::new()
    };
    let mut response = aws_smithy_runtime_api::http::Response::new(
        aws_smithy_runtime_api::http::StatusCode::try_from(status).ok()?,
        ::aws_smithy_types::body::SdkBody::from(body),
    );
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        crate::state::ADDON_MARKETPLACE_LISTING_MEDIA_TYPE,
    );
    headers.insert("content-length", length.to_string());
    // The page is served from this emulator's own origin, so the type it announces is
    // the only type it may be treated as.
    headers.insert("x-content-type-options", "nosniff");
    Some(response)
}
