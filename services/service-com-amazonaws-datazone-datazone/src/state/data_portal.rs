// SPDX-License-Identifier: CC0-1.0

//! A domain's DATA PORTAL: the address this emulator publishes for it, the route that
//! answers there, and the page a fetch of that address renders.
//!
//! Every response that reports a domain's portal URL ("The data portal URL for the Amazon
//! DataZone domain", `smithy_query(query="doc", shape="DomainSummary", member="portalUrl")`)
//! hands the caller an address to FOLLOW. AWS serves a domain's portal on a host of its own,
//! which this emulator neither owns nor answers on, so a caller that followed such an address
//! would leave the emulator altogether — reaching nothing in a sandbox, and a real AWS
//! endpoint that knows nothing of the emulated domain anywhere else.
//!
//! So the address is the endpoint the request arrived on
//! ([`::smithy_server_runtime::OperationContext::endpoint`], "the base URL a later request
//! addresses") plus the path this module lays out, under the reserved namespace every
//! emulator-served plane is addressed in
//! (`::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`), with the DOMAIN's own
//! id as the segment that says which portal it is — and `crate::service` SERVES exactly that
//! path from the stored domain.
//!
//! One function mints the URL ([`data_portal_url`]) and one reads the path back
//! ([`data_portal_route`]), so what a response advertises and what this service answers
//! cannot drift apart.

use super::Domain;

/// The segment that names this service's data-portal plane, behind the reserved namespace
/// segment: `/_aws/datazone-portal/{domainId}`.
///
/// Shared by the URL the domain reports and by the route that serves it, so the address
/// this service publishes is the address it answers on.
pub const DATA_PORTAL_PLANE: &str = "datazone-portal";

/// The request path the data portal of `domain_id` is served at.
///
/// The namespace segment and the split into segments are
/// `::service_common::data_plane_addressing`'s, so this plane is addressed the way every
/// other emulator-served plane is. A `DomainId` is admitted only by
/// `^dzd[-_][a-zA-Z0-9_-]{1,36}$` (the model enforces it at the request boundary), so every
/// character it can carry is already a path character and the id needs no escaping.
#[must_use]
pub fn data_portal_path(domain_id: &str) -> String {
    format!(
        "/{}/{DATA_PORTAL_PLANE}/{domain_id}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// Where a client reaches the data portal of `domain_id`: the endpoint THIS emulator is
/// served at (`endpoint_base`) followed by that portal's own path.
///
/// The host is never one of AWS's own names — a URL a caller is told to follow has to lead
/// back to where that caller is already talking — and the per-domain part is the domain's
/// own id, so each domain has a portal address of its own.
#[must_use]
pub fn data_portal_url(endpoint_base: &str, domain_id: &str) -> String {
    format!(
        "{}{}",
        endpoint_base.trim_end_matches('/'),
        data_portal_path(domain_id)
    )
}

/// The domain whose data portal a request `uri` addresses, for a path of the form
/// [`data_portal_path`] mints, and `None` for every other path.
///
/// The inverse of the minting above, read through the shared addressing module, so a URL
/// this service published resolves back to the domain behind it. A trailing `/` is the same
/// address (that is how a browser normalises a bare path), and nothing below the portal's
/// own segment is this route: the portal is one page, not a path space.
#[must_use]
pub fn data_portal_route(uri: &str) -> Option<&str> {
    use ::service_common::data_plane_addressing::{first_segment, EMULATOR_PATH_NAMESPACE};
    let path = request_path(uri);
    let (namespace, rest) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != DATA_PORTAL_PLANE {
        return None;
    }
    let (domain_id, below) = first_segment(rest)?;
    (!domain_id.is_empty() && below == "/").then_some(domain_id)
}

/// The PATH of a request target, whether it arrived in origin form (`/a/b?q`) or in
/// absolute form (`https://host/a/b?q`), which is how a request that carries its endpoint in
/// the URI spells the same address.
fn request_path(uri: &str) -> &str {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    match path.split_once("://") {
        Some((_scheme, authority_and_path)) => authority_and_path
            .find('/')
            .map_or("/", |at| &authority_and_path[at..]),
        None => path,
    }
}

/// The media type the data portal is served as: a web page, served from this emulator's own
/// origin.
pub const DATA_PORTAL_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The page a fetch of a domain's portal address renders: the domain this portal belongs to,
/// as the store holds it right now — its id, its name and the status it currently reports.
///
/// Every value comes from the stored record, so the page a caller opens cannot disagree with
/// what `GetDomain` reports about the same domain. The name is a caller-supplied string, so
/// it is written into the document through the HTML escaping of the `html-escape` crate
/// rather than inlined.
#[must_use]
pub fn data_portal_page(domain: &Domain) -> String {
    let title = format!("Amazon DataZone data portal — {}", domain.name);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n<h1>{name}</h1>\n\
         <dl>\n<dt>Domain</dt><dd>{id}</dd>\n<dt>Status</dt><dd>{status}</dd>\n</dl>\n\
         </body>\n</html>\n",
        title = escaped(&title),
        name = escaped(&domain.name),
        id = escaped(&domain.id),
        status = escaped(&domain.status),
    )
}

/// The page a fetch of a portal address this emulator holds no domain for renders: a domain
/// that was never created, or one whose deletion has completed.
#[must_use]
pub fn data_portal_not_found_page() -> String {
    "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
     <title>Amazon DataZone data portal</title>\n</head>\n<body>\n\
     <h1>No such data portal</h1>\n</body>\n</html>\n"
        .to_string()
}

/// `value` as text inside an HTML element: the `html-escape` crate's own escaping, so no
/// stored string can open or close an element of the page it is written into.
fn escaped(value: &str) -> String {
    ::html_escape::encode_safe(value).into_owned()
}
