// SPDX-License-Identifier: CC0-1.0

//! The ROUTE that answers at a domain's announced endpoint.
//!
//! A domain reports `domainEndpointUrl` and `customEndpointUrls` for a caller to OPEN, and
//! this emulator publishes both on its own endpoint
//! ([`crate::state::domain_endpoint_url`], [`crate::state::custom_endpoint_urls`]). This
//! module is the other half of those addresses: the claim that says a fetch of one is this
//! service's to answer, and the answer itself — the page of the stored domain the path names
//! ([`crate::state::domain_endpoint_page`]), and a not-found page for a path naming a domain
//! this emulator does not hold.
//!
//! The fetch carries no credential and names no modelled operation: a domain endpoint is
//! opened by whatever browser, probe or test holds the URL, exactly as AWS serves its own
//! domain host. So the claim is on the PATH this service itself minted, and the answer is a
//! web page rather than a modelled API envelope.

use smithy_server_runtime::http;

/// Whether `parts` addresses a domain endpoint this service publishes, for a request that
/// resolved no routing key of its own: a `GET` (or the `HEAD` that asks only what is there)
/// of exactly the path [`crate::state::domain_endpoint_path`] or
/// [`crate::state::domain_name_endpoint_path`] mints.
pub fn claimed(parts: &http::request::Parts) -> bool {
    (parts.method == http::Method::GET || parts.method == http::Method::HEAD)
        && crate::state::domain_endpoint_route(&parts.uri.to_string()).is_some()
}

/// The page a fetch of `req` renders, or `None` when `req` addresses no domain endpoint of
/// this service — which leaves the request to the answer it would otherwise get.
///
/// A pure READ of the store: opening a domain endpoint changes nothing a later caller sees,
/// so the page states what `GetDomain` already reports about the same domain.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body_wanted = match req.method() {
        "GET" => true,
        "HEAD" => false,
        _ => return None,
    };
    let route = crate::state::domain_endpoint_route(req.uri())?;
    let state = crate::state::service_state(engine)?;
    let page = {
        let store = state.lock();
        store
            .domain_at_endpoint(route)
            .map(crate::state::domain_endpoint_page)
    };
    // The endpoint of a domain this emulator holds, and a page saying there is none for every
    // other address of this shape: nothing is invented for a domain that was never created or
    // whose deletion has completed.
    let status = if page.is_some() { 200u16 } else { 404 };
    let document = page.unwrap_or_else(crate::state::domain_endpoint_not_found_page);
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
    headers.insert("content-type", crate::state::DOMAIN_ENDPOINT_MEDIA_TYPE);
    // The size of the page itself, which is what a `HEAD` of it reports with no body.
    headers.insert("content-length", document.len().to_string());
    // The page is served from this emulator's own origin, so the type it announces is the
    // only type it may be treated as.
    headers.insert("x-content-type-options", "nosniff");
    Some(response)
}
