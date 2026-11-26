// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS the OAuth consent step of one app authorization is served at, and
//! the page that answers there.
//!
//! An `oauth2` app authorization reports `authUrl` ("The application URL for the
//! OAuth flow") for a person to OPEN: the consent step is performed there, and
//! what it hands back is what `ConnectAppAuthorization` takes in its
//! `authRequest` ("after permission is granted in the application OAuth page").
//! AWS performs that step on the application's own host, which this emulator
//! neither owns nor answers on, so the address this emulator publishes is its
//! OWN — the endpoint the create call arrived at, followed by the path minted
//! here — and this module is the other half of it: the page the fetch of that
//! address renders, out of the stored authorization alone.
//!
//! The URL is composed ONCE, by the create call that mints the authorization
//! ([`super::AppAuthorizationRecord::auth_url`] reports what was stored), so
//! every later read serializes the same bytes rather than re-composing them.

use std::time::SystemTime;

use super::AppAuthorizationRecord;

/// The segment that names this service's OAUTH CONSENT plane, behind the
/// reserved namespace segment every plane this emulator serves at its own front
/// door is addressed under
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`):
/// `/_aws/appfabric-oauth-consent/{appBundleId}/{appAuthorizationId}`.
///
/// Shared by the URL an authorization reports and by the route that answers
/// there, so the address this service publishes is the address it serves.
pub const OAUTH_CONSENT_PLANE_SEGMENT: &str = "appfabric-oauth-consent";

/// The request path the consent step of the app authorization `authorization_id`
/// of the app bundle `app_bundle_id` is served at.
///
/// Both ids are the bare UUIDs the two records are stored under (the model's
/// `UUID` shape, `^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`),
/// so every character they can carry is already a path character and neither
/// needs escaping.
pub fn oauth_consent_path(app_bundle_id: &str, authorization_id: &str) -> String {
    format!(
        "/{}/{OAUTH_CONSENT_PLANE_SEGMENT}/{app_bundle_id}/{authorization_id}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// Where the consent step of ONE app authorization is performed: the endpoint
/// THIS emulator is served at (`endpoint_base`, what `ctx.endpoint()` answers),
/// followed by that authorization's own path.
///
/// Composed by the create call and STORED on the record, never rebuilt by a
/// read: `authUrl` is an address a caller holds on to between the create call
/// and the consent step, so the bytes it reports have to be the same ones on
/// every later read of the same authorization.
pub fn oauth_consent_url(
    endpoint_base: &str,
    app_bundle_id: &str,
    authorization_id: &str,
) -> String {
    format!(
        "{}{}",
        endpoint_base.trim_end_matches('/'),
        oauth_consent_path(app_bundle_id, authorization_id)
    )
}

/// The `(appBundleId, appAuthorizationId)` pair a request `uri` addresses, for a
/// path of the form [`oauth_consent_path`] mints, and `None` for every other
/// path.
///
/// The inverse of the minting above, read through the shared addressing module,
/// so a URL this service published resolves back to the authorization behind it.
/// A trailing `/` is the same address (that is how a browser normalises a bare
/// path), and nothing below the authorization's own segment is this route: the
/// consent step is one page, not a path space.
pub fn oauth_consent_route(uri: &str) -> Option<(&str, &str)> {
    use ::service_common::data_plane_addressing::{first_segment, EMULATOR_PATH_NAMESPACE};
    let path = request_target_path(uri);
    let (namespace, rest) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != OAUTH_CONSENT_PLANE_SEGMENT {
        return None;
    }
    let (app_bundle_id, rest) = first_segment(rest)?;
    let (authorization_id, below) = first_segment(rest)?;
    (!app_bundle_id.is_empty() && !authorization_id.is_empty() && below == "/")
        .then_some((app_bundle_id, authorization_id))
}

/// The PATH of a request target, whether it arrived in origin form (`/a/b?q`) or
/// in absolute form (`https://host/a/b?q`), which is how a request that carries
/// its endpoint in the URI spells the same address.
fn request_target_path(uri: &str) -> &str {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    match path.split_once("://") {
        Some((_scheme, authority_and_path)) => authority_and_path
            .find('/')
            .map_or("/", |at| &authority_and_path[at..]),
        None => path,
    }
}

/// The media type the consent step is served as: a web page, served from this
/// emulator's own origin.
pub const OAUTH_CONSENT_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The page a fetch of an authorization's consent address renders: the
/// authorization that address belongs to, as the store holds it right now — the
/// application and tenant permission is being granted for, the authorization's
/// own identifier, and the status it currently reports.
///
/// Every value comes from the stored record, so the page cannot disagree with
/// what `GetAppAuthorization` reports about the same authorization. It carries
/// no credential material: what the create call supplied is not part of the
/// record (see [`AppAuthorizationRecord`]). The application and tenant names are
/// caller-supplied strings, so they are written into the document through the
/// escaping of the `html-escape` crate rather than inlined.
pub fn oauth_consent_page(record: &AppAuthorizationRecord, now: SystemTime) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n<h1>{app}</h1>\n\
         <dl>\n<dt>App authorization</dt><dd>{id}</dd>\n\
         <dt>Tenant</dt><dd>{tenant} ({tenant_id})</dd>\n\
         <dt>Authorization type</dt><dd>{auth_type}</dd>\n\
         <dt>Status</dt><dd>{status}</dd>\n</dl>\n</body>\n</html>\n",
        title = escaped(&format!("AppFabric OAuth consent — {}", record.app)),
        app = escaped(&record.app),
        id = escaped(&record.id),
        tenant = escaped(&record.tenant_display_name),
        tenant_id = escaped(&record.tenant_identifier),
        auth_type = escaped(&record.auth_type),
        status = escaped(record.status_at(now)),
    )
}

/// The page a fetch of a consent address this emulator holds no app
/// authorization for renders: one that was never created, or one a delete has
/// removed.
pub fn oauth_consent_not_found_page() -> String {
    "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
     <title>AppFabric OAuth consent</title>\n</head>\n<body>\n\
     <h1>No such app authorization</h1>\n</body>\n</html>\n"
        .to_string()
}

/// `value` as text inside an HTML element: the `html-escape` crate's own
/// escaping, so no stored string can open or close an element of the page it is
/// written into.
fn escaped(value: &str) -> String {
    ::html_escape::encode_safe(value).into_owned()
}
