// SPDX-License-Identifier: CC0-1.0

//! The ROUTE that answers at a domain's announced endpoint.
//!
//! A domain reports `domainEndpointUrl` and `customEndpointUrls` for a caller to OPEN, and
//! this emulator announces both as hosts whose leftmost label is the domain's own identity,
//! under a suffix of its own ([`crate::state::domain_endpoint_url`],
//! [`crate::state::custom_endpoint_urls`]). This module is the other half of those
//! addresses: the claim that says a fetch of one is this service's to answer, and the answer
//! itself — the page of the stored domain the address names
//! ([`crate::state::domain_endpoint_page`]), and a not-found page for a local path naming a
//! domain this emulator does not hold.
//!
//! The fetch carries no credential and names no modelled operation: a domain endpoint is
//! opened by whatever browser, probe or test holds the URL, exactly as AWS serves its own
//! domain host. So the claim is on the ADDRESS this service itself minted, and the answer is
//! a web page rather than a modelled API envelope.
//!
//! The two addressings are claimed differently, because they say different things. The LOCAL
//! PATH form is this service's alone — the reserved data-plane namespace and this service's
//! own plane segment — so a fetch of it is claimed whatever host the client addressed, and a
//! path naming a domain this emulator does not hold is answered by this service too. An
//! announced HOST, by contrast, names no service at all: its first label is a resource
//! identity, and other planes of this emulator announce hosts of the same shape. A fetch of
//! one is therefore claimed only when THIS emulator holds the domain that label names, so a
//! host belonging to another plane's resource is left to that plane.

use smithy_server_runtime::http;

/// Whether `parts` addresses a domain endpoint this service publishes, for a request that
/// resolved no routing key of its own: a `GET` (or the `HEAD` that asks only what is there)
/// of an address [`crate::state::addressed_domain_endpoint`] reads back — with an announced
/// HOST claimed only when `engine` holds the domain its first label names (see the module
/// docs).
pub fn claimed<Eng: Send + Sync + 'static>(engine: &Eng, parts: &http::request::Parts) -> bool {
    if parts.method != http::Method::GET && parts.method != http::Method::HEAD {
        return false;
    }
    let uri = parts.uri.to_string();
    let host = parts
        .headers
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok());
    match crate::state::addressed_domain_endpoint(&uri, host) {
        Some(crate::state::DomainEndpointRoute::ByHostLabel(label)) => holds_domain(
            engine,
            crate::state::DomainEndpointRoute::ByHostLabel(label),
        ),
        Some(_) => true,
        None => false,
    }
}

/// Whether this emulator holds the domain `route` addresses.
fn holds_domain<Eng: Send + Sync + 'static>(
    engine: &Eng,
    route: crate::state::DomainEndpointRoute<'_>,
) -> bool {
    crate::state::service_state(engine).is_some_and(|state| {
        let store = state.lock();
        store.domain_at_endpoint(route).is_some()
    })
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
    let route = crate::state::addressed_domain_endpoint(req.uri(), req.headers().get("host"))?;
    let state = crate::state::service_state(engine)?;
    let page = {
        let store = state.lock();
        store
            .domain_at_endpoint(route)
            .map(crate::state::domain_endpoint_page)
    };
    // The endpoint of a domain this emulator holds, and a page saying there is none for every
    // other LOCAL PATH of this shape: nothing is invented for a domain that was never created
    // or whose deletion has completed. An announced HOST this emulator holds no domain for is
    // not this service's address at all, so it keeps the answer it would otherwise get.
    if page.is_none() && matches!(route, crate::state::DomainEndpointRoute::ByHostLabel(_)) {
        return None;
    }
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
