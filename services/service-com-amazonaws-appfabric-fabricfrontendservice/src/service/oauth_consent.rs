// SPDX-License-Identifier: CC0-1.0

//! The ROUTE that answers at an app authorization's published consent address.
//!
//! An `oauth2` app authorization reports `authUrl` for a person to FOLLOW, and
//! this emulator publishes that address on its own endpoint
//! ([`crate::state::oauth_consent_url`]). This module is the other half of that
//! address: the claim that says a fetch of it is this service's to answer, and
//! the answer itself — the page of the stored authorization the path names
//! ([`crate::state::oauth_consent_page`]), and a not-found page for a path
//! naming an authorization this emulator does not hold.
//!
//! The fetch carries no credential and names no modelled operation: a consent
//! URL is opened by whatever browser, probe or test holds it, exactly as AWS
//! serves the application's own consent host. So the claim is on the PATH this
//! service itself minted, and the answer is a web page rather than a modelled
//! API envelope.

use smithy_server_runtime::http;

/// The status a fetch of a published consent address is answered under: the page
/// of an authorization this emulator holds, and a page saying there is none for
/// every other address of that shape — nothing is invented for an authorization
/// that was never created, or that a delete has removed.
const PAGE_FOUND: u16 = 200;
const PAGE_NOT_FOUND: u16 = 404;

/// Whether `parts` addresses the consent page of an app authorization this
/// service publishes, for a request that resolved no routing key of its own: a
/// `GET` (or the `HEAD` that asks only what is there) of exactly the path
/// [`crate::state::oauth_consent_path`] mints.
pub fn claimed(parts: &http::request::Parts) -> bool {
    (parts.method == http::Method::GET || parts.method == http::Method::HEAD)
        && crate::state::oauth_consent_route(&parts.uri.to_string()).is_some()
}

/// The consent page a fetch of `req` renders, or `None` when `req` addresses no
/// app authorization of this service — which leaves the request to the answer it
/// would otherwise get.
///
/// A READ of the store: opening the page changes nothing a later caller sees, so
/// it states the status the authorization already reports rather than moving it.
/// The consent step's own outcome is what `ConnectAppAuthorization` carries.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body_wanted = match req.method() {
        "GET" => true,
        "HEAD" => false,
        _ => return None,
    };
    let (app_bundle_id, authorization_id) = crate::state::oauth_consent_route(req.uri())?;
    let state = crate::state::service_state(engine)?;
    let now = ::std::time::SystemTime::now();
    let page = {
        let store = state.lock();
        store
            .app_bundles
            .get(app_bundle_id)
            .and_then(|bundle| bundle.app_authorizations.get(authorization_id))
            .map(|authorization| crate::state::oauth_consent_page(authorization, now))
    };
    let status = if page.is_some() {
        PAGE_FOUND
    } else {
        PAGE_NOT_FOUND
    };
    let document = page.unwrap_or_else(crate::state::oauth_consent_not_found_page);
    served_page(status, crate::state::OAUTH_CONSENT_MEDIA_TYPE, &document, body_wanted)
}

/// The response a fetch of the consent page is served in: `document` as a web
/// page of `media_type`, under `status`, with the body omitted for the `HEAD`
/// that asks only what is there (`body_wanted` false) while still reporting the
/// page's own size.
///
/// `None` only when `status` is not a status code at all, which leaves the
/// request to the answer it would otherwise get.
fn served_page(
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
    // The size of the page itself, which is what a `HEAD` of it reports with no
    // body.
    headers.insert("content-length", document.len().to_string());
    // The page is served from this emulator's own origin, so the type it
    // announces is the only type it may be treated as.
    headers.insert("x-content-type-options", "nosniff");
    Some(response)
}
