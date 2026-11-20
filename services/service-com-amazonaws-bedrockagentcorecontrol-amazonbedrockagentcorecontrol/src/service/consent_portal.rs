// SPDX-License-Identifier: CC0-1.0

//! The ROUTE that answers at a consent portal's published address.
//!
//! A consent portal reports `portalUrl` for an end user to FOLLOW, and this emulator
//! publishes that address on its own endpoint ([`crate::state::consent_portal_url`]). This
//! module is the other half of that address: the claim that says a fetch of it is this
//! service's to answer, and the answer itself — the page of the stored portal the path names
//! ([`crate::state::consent_portal_page`]), and a not-found page for a path naming a portal
//! this emulator does not hold.
//!
//! The fetch carries no credential and names no modelled operation: a portal URL is opened
//! by whatever browser, probe or test holds it, exactly as AWS serves its own portal host.
//! So the claim is on the PATH this service itself minted, and the answer is a web page
//! rather than a modelled API envelope.

use smithy_server_runtime::http;

/// Whether `parts` addresses a consent portal this service publishes, for a request that
/// resolved no routing key of its own: a `GET` (or the `HEAD` that asks only what is there)
/// of exactly the path [`crate::state::consent_portal_path`] mints.
pub fn claimed(parts: &http::request::Parts) -> bool {
    (parts.method == http::Method::GET || parts.method == http::Method::HEAD)
        && crate::state::consent_portal_route(&parts.uri.to_string()).is_some()
}

/// The portal page a fetch of `req` renders, or `None` when `req` addresses no consent
/// portal of this service — which leaves the request to the answer it would otherwise get.
///
/// A READ of the store, apart from the clock-owed removal every reader of a consent portal
/// materialises
/// ([`crate::state::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::expire_deleted_consent_portals`]),
/// so opening a portal changes nothing a later caller sees: the page states the status the
/// portal already reports.
pub fn answer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &aws_smithy_runtime_api::http::Request,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let body_wanted = match req.method() {
        "GET" => true,
        "HEAD" => false,
        _ => return None,
    };
    let consent_portal_id = crate::state::consent_portal_route(req.uri())?;
    let state = crate::state::service_state(engine)?;
    let now = ::std::time::SystemTime::now();
    let page = {
        let mut store = state.lock();
        store.expire_deleted_consent_portals(now);
        store
            .consent_portals
            .get(consent_portal_id)
            .map(|record| crate::state::consent_portal_page(record, now))
    };
    // The page of a portal this emulator holds, and a page saying there is none for every
    // other address of this shape: nothing is invented for a portal that was never created
    // or whose deletion has completed.
    let status = if page.is_some() {
        super::served_page::PAGE_FOUND
    } else {
        super::served_page::PAGE_NOT_FOUND
    };
    let document = page.unwrap_or_else(crate::state::consent_portal_not_found_page);
    super::served_page::served_page(
        status,
        crate::state::CONSENT_PORTAL_MEDIA_TYPE,
        &document,
        body_wanted,
    )
}
