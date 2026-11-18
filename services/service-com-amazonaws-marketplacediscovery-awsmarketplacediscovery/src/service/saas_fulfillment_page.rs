// SPDX-License-Identifier: CC0-1.0

//! The ROUTE that answers at a SaaS fulfillment option's published addresses.
//!
//! A SaaS fulfillment option reports `fulfillmentUrl` and `launchUrl` for a BUYER to follow,
//! and this emulator publishes both on its own endpoint
//! ([`crate::state::saas_page_url`]). This module is the other half of that address: the
//! claim that says a fetch of it is this service's to answer, and the answer itself — the
//! page of the stored option the path names
//! ([`crate::state::ComAmazonawsMarketplacediscoveryAwsmarketplacediscoveryState::saas_fulfillment_page`]),
//! and a not-found page for an address naming no option this emulator holds a page for.
//!
//! The fetch carries no credential and names no modelled operation: a registration or launch
//! address is opened by whatever browser, probe or test holds it, exactly as AWS serves the
//! seller's own host. So the claim is on the PATH this service itself minted, and the answer
//! is a web page rather than a modelled API envelope.

use smithy_server_runtime::http;

/// Whether `parts` addresses a buyer-facing page this service publishes, for a request that
/// resolved no routing key of its own: a `GET` (or the `HEAD` that asks only what is there)
/// of exactly a path [`crate::state::saas_page_path`] mints.
pub fn claimed(parts: &http::request::Parts) -> bool {
    (parts.method == http::Method::GET || parts.method == http::Method::HEAD)
        && crate::state::saas_page_route(&parts.uri.to_string()).is_some()
}

/// The page a fetch of `req` renders, or `None` when `req` addresses no buyer-facing page of
/// this service — which leaves the request to the answer it would otherwise get.
///
/// A READ of the catalog, which nothing mutates after seeding, so opening a published address
/// changes nothing a later caller sees: the page restates the option
/// `ListFulfillmentOptions` reports.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body_wanted = match req.method() {
        "GET" => true,
        "HEAD" => false,
        _ => return None,
    };
    let (fulfillment_option_id, page) = crate::state::saas_page_route(req.uri())?;
    let state = crate::state::service_state(engine)?;
    let document = state
        .lock()
        .saas_fulfillment_page(fulfillment_option_id, page);
    // The page of an option this emulator published that address for, and a page saying
    // there is none for every other address of this shape: nothing is invented for an
    // option the catalog does not hold, or one whose record names no such page.
    let status = if document.is_some() { 200_u16 } else { 404 };
    let document = document.unwrap_or_else(crate::state::saas_page_not_found_page);
    let body = if body_wanted {
        document.clone().into_bytes()
    } else {
        Vec::new()
    };
    let mut response = aws_smithy_runtime_api::http::Response::new(
        aws_smithy_runtime_api::http::StatusCode::try_from(status).ok()?,
        ::aws_smithy_types::body::SdkBody::from(body),
    );
    let headers = response.headers_mut();
    headers.insert("content-type", crate::state::SAAS_PAGE_MEDIA_TYPE);
    // The size of the page itself, which is what a `HEAD` of it reports with no body.
    headers.insert("content-length", document.len().to_string());
    // The page is served from this emulator's own origin, so the type it announces is the
    // only type it may be treated as.
    headers.insert("x-content-type-options", "nosniff");
    Some(response)
}
