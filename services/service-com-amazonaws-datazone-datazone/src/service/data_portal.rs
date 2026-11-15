// SPDX-License-Identifier: CC0-1.0

//! The ROUTE that answers at a domain's data portal address.
//!
//! A domain reports `portalUrl` for a caller to FOLLOW, and this emulator publishes that
//! address on its own endpoint ([`crate::state::data_portal_url`]). This module is the other
//! half of that address: the claim that says a fetch of it is this service's to answer, and
//! the answer itself — the portal page of the stored domain the path names
//! ([`crate::state::data_portal_page`]), and a not-found page for a path naming a domain this
//! emulator does not hold.
//!
//! The fetch carries no credential and names no modelled operation: a portal URL is opened by
//! whatever browser, probe or test holds it, exactly as AWS serves its own portal host. So the
//! claim is on the PATH this service itself minted, and the answer is a web page rather than a
//! modelled API envelope.

use smithy_server_runtime::http;

/// Whether `parts` addresses a data portal this service publishes, for a request that resolved
/// no routing key of its own: a `GET` (or the `HEAD` that asks only what is there) of exactly
/// the path [`crate::state::data_portal_path`] mints.
pub fn claimed(parts: &http::request::Parts) -> bool {
    (parts.method == http::Method::GET || parts.method == http::Method::HEAD)
        && crate::state::data_portal_route(&parts.uri.to_string()).is_some()
}

/// The portal page a fetch of `req` renders, or `None` when `req` addresses no data portal of
/// this service — which leaves the request to the answer it would otherwise get.
///
/// A pure READ of the store, apart from the clock-owed domain transitions every reader of this
/// store materialises ([`crate::state::ComAmazonawsDatazoneDatazoneState::settle_clock`]), so
/// opening a portal changes nothing a later caller sees: the page states the status the domain
/// already reports.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body_wanted = match req.method() {
        "GET" => true,
        "HEAD" => false,
        _ => return None,
    };
    let domain_id = crate::state::data_portal_route(req.uri())?;
    let state = crate::state::service_state(engine)?;
    let page = {
        let mut store = state.lock();
        store.settle_clock(crate::state::now_ms());
        store
            .domains
            .get(domain_id)
            .map(crate::state::data_portal_page)
    };
    // The portal of a domain this emulator holds, and a page saying there is none for every
    // other address of this shape: nothing is invented for a domain that was never created or
    // whose deletion has completed.
    let status = if page.is_some() { 200u16 } else { 404 };
    let document = page.unwrap_or_else(crate::state::data_portal_not_found_page);
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
    headers.insert("content-type", crate::state::DATA_PORTAL_MEDIA_TYPE);
    // The size of the page itself, which is what a `HEAD` of it reports with no body.
    headers.insert("content-length", document.len().to_string());
    // The page is served from this emulator's own origin, so the type it announces is the only
    // type it may be treated as.
    headers.insert("x-content-type-options", "nosniff");
    Some(response)
}
