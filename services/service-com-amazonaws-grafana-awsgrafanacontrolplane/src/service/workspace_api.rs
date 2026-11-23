// SPDX-License-Identifier: CC0-1.0

//! The workspace's own (native) Grafana HTTP API: the data plane the endpoint
//! `DescribeWorkspace` reports answers on.
//!
//! `CreateWorkspaceApiKey` mints a key "used to authenticate requests sent to the
//! workspace's HTTP API", and `CreateWorkspaceServiceAccountToken` mints a key
//! "used when calling Grafana APIs". That API is not in the Smithy model — the
//! model describes the control plane only — so it names no modelled operation and
//! is served from [`crate::service::EmulatedService::override_routing_error`],
//! exactly as `service-com-amazonaws-codecommit-codecommit-20150413` serves git's
//! smart-HTTP transport.
//!
//! The address it answers on is the one [`crate::state::workspace_endpoint`]
//! announces: the bare host `<workspaceId>.grafana-workspace.<region>.amazonaws.com`
//! the control plane reports as `WorkspaceDescription$endpoint`. A client that reads
//! `endpoint`, resolves it to this emulator and appends a Grafana route reaches this
//! module:
//!
//! | request | meaning |
//! |---|---|
//! | `GET https://<endpoint>/api/org` | the workspace's organization: its `id` and `name` |
//!
//! Every other route under the endpoint is answered with **501**, never an empty
//! 200: a client cannot tell an unimplemented route from an implemented one that
//! has nothing to say.
//!
//! # Authentication
//!
//! It verifies no request signature, and cannot: a caller presents
//! `Authorization: Bearer <key>` — an API key or a service-account token, which
//! are not SigV4 credentials — so there is nothing for the framework's SigV4
//! authenticator to check, and refusing every such request instead is exactly the
//! defect this data plane fixes. A presented token is recognised by its
//! [`crate::state::bearer_token_digest`], held on the key record the control plane
//! wrote, so the store never holds the token itself. A token the workspace no
//! longer holds — deleted, expired, or belonging to a deleted workspace or
//! disabled service account — authenticates nothing, which is what makes
//! revocation observable at the destination.
//!
//! Both faces of the service read ONE store: the keys this module authenticates
//! are the records `CreateWorkspaceApiKey` / `CreateWorkspaceServiceAccountToken`
//! persisted and `DeleteWorkspaceApiKey` removes, and a service-account token that
//! authorizes here has its `lastUsedAt` set, which
//! `ListWorkspaceServiceAccountTokens` then reports.

use std::time::SystemTime;

use crate::state::{
    ComAmazonawsGrafanaAwsgrafanacontrolplaneState as Store, Workspace,
    WORKSPACE_ACTIVE as ACTIVE,
};

/// The id of the one organization a workspace's Grafana has.
///
/// Grafana numbers organizations from 1 and a workspace is a single Grafana
/// server with a single organization, so that organization is organization 1.
const WORKSPACE_ORG_ID: u64 = 1;

/// What one request to a workspace's Grafana HTTP API named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The workspace whose endpoint was addressed.
    pub workspace_id: String,
    /// The route it asked for.
    pub route: Route,
}

/// Which route of the native API a request named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// `GET /api/org`: the organization of this workspace.
    Org,
    /// A route under the workspace endpoint this emulator does not serve, kept so
    /// the refusal can name what was asked for.
    Unserved { method: String, path: String },
}

/// One rendered answer: JSON, like every Grafana HTTP API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The content type every answer below carries.
pub const CONTENT_TYPE: &str = "application/json";

/// The request a `method`, `host` and `path` name, or `None` when the request is
/// not addressed to a workspace endpoint this service announces.
///
/// A request is this data plane's when its `Host` is the endpoint host of a
/// workspace ([`crate::state::workspace_id_at_endpoint_host`]) — the address the
/// control plane announced for it. The host is the one thing that identifies such
/// a request: a Grafana client sends no SigV4 credential and no `X-Amz-Target`, so
/// it is also what [`crate::service::EmulatedService::can_handle`] claims on. The
/// whole path is then the Grafana route, because the endpoint is a host and
/// carries no path of its own.
pub fn route(method: &str, host: Option<&str>, path: &str) -> Option<Request> {
    let workspace_id = crate::state::workspace_id_at_endpoint_host(host?)?;
    let route = if method.eq_ignore_ascii_case("GET") && path == "/api/org" {
        Route::Org
    } else {
        Route::Unserved {
            method: method.to_string(),
            path: path.to_string(),
        }
    };
    Some(Request {
        workspace_id,
        route,
    })
}

/// The authority a request addresses: its `Host` header, or — on HTTP/2 and an
/// absolute-form request line, which carry none — the authority of its URI.
///
/// Each source is read in that order and the first one that names an address wins,
/// so a `Host` header carrying nothing leaves the URI's authority still to be
/// read. It is what [`route`] matches a workspace endpoint against.
pub fn addressed_host<'a>(
    host_header: Option<&'a str>,
    uri_authority: Option<&'a str>,
) -> Option<&'a str> {
    [host_header, uri_authority]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
}

/// The path of a URI that may be absolute or origin-form.
///
/// Parsed by the `http` crate, which is what the rest of this service's routing
/// reads a URI with (`can_handle` uses `parts.uri.path()`), so an absolute form
/// with no path, userinfo, or a fragment is split by the parser rather than by
/// hand. A request target the parser does not accept falls back to the raw string
/// with its query and fragment cut off, so such a request is still routed by its
/// path rather than silently unclaimed.
pub fn uri_path(uri: &str) -> String {
    if let Ok(parsed) = uri.parse::<::http::Uri>() {
        return parsed.path().to_string();
    }
    let without_fragment = uri.split_once('#').map_or(uri, |(before, _)| before);
    without_fragment
        .split_once('?')
        .map_or(without_fragment, |(path, _)| path)
        .to_string()
}

/// Serve one native-API request from this service's own state.
///
/// `authorization` is the request's `Authorization` header, which is where the
/// bearer key travels. `now` is the instant the request is served at: it settles
/// what the clock owes the workspace (an expired API key is gone, a due status
/// change has happened) before anything is read, so a key past its
/// `secondsToLive` authenticates nothing even if no control-plane call has run
/// since it expired.
pub fn serve(
    store: &mut Store,
    request: &Request,
    authorization: Option<&str>,
    now: SystemTime,
) -> Response {
    let Some(workspace) = store.settled_mut(&request.workspace_id, now) else {
        // The address exists (it is this emulator's own), but no workspace
        // answers on it: one that was never created, or has been deleted.
        return json(
            404,
            &format!(
                "no workspace {} is served at this endpoint",
                request.workspace_id
            ),
        );
    };

    // A workspace announces its endpoint from the moment it is created, but its
    // Grafana server is running only while the workspace is ACTIVE — not before it
    // is up, and not once a deletion has been accepted for it.
    if workspace.status != ACTIVE {
        return json(
            503,
            &format!(
                "workspace {} is {} and its Grafana HTTP API is not serving",
                workspace.id, workspace.status
            ),
        );
    }

    let Some(presented) = bearer_token(authorization) else {
        return json(401, "Unauthorized");
    };
    if !authenticates(workspace, &presented, now) {
        return json(401, "Unauthorized");
    }

    match &request.route {
        Route::Org => org_response(workspace),
        // The caller authenticated; the route is simply not written yet. 501
        // rather than an empty 200, for the reason `crate::scaffolded` gives.
        Route::Unserved { method, path } => {
            // A request to the endpoint itself carries no route path at all; it
            // still names the Grafana API's root.
            let path = if path.is_empty() { "/" } else { path.as_str() };
            json(
                501,
                &format!(
                    "{method} {path} of the Grafana HTTP API is not implemented in this \
                     emulator"
                ),
            )
        }
    }
}

/// The bearer token an `Authorization` header presents, or `None` when it carries
/// no non-empty `Bearer` credential.
fn bearer_token(authorization: Option<&str>) -> Option<String> {
    let value = authorization?.trim();
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Whether `presented` is a live bearer credential of this workspace: one of its
/// API keys, or a token of one of its service accounts — the two credentials the
/// control plane mints for calling the workspace's Grafana HTTP API.
///
/// A service-account token that authorizes has its `lastUsedAt` set to `now`:
/// `ServiceAccountTokenSummary$lastUsedAt` is "[t]he last time the token was used
/// to authorize a Grafana HTTP API", and this is that use.
fn authenticates(workspace: &mut Workspace, presented: &str, now: SystemTime) -> bool {
    let digest = crate::state::bearer_token_digest(presented);

    // `settle` has already dropped every API key past its `secondsToLive`, so a
    // key still on the record is a live key.
    if workspace
        .api_keys
        .values()
        .any(|key| crate::state::same_token_digest(&key.token_digest, &digest))
    {
        return true;
    }

    // A service-account token is kept past its expiry (its `expiresAt` is
    // reported), so expiry is checked here rather than by removal; a disabled
    // account authorizes nothing either.
    for account in &mut workspace.service_accounts {
        if account.disabled {
            continue;
        }
        for token in &mut account.tokens {
            if now < token.expires
                && crate::state::same_token_digest(&token.token_digest, &digest)
            {
                token.last_used = Some(now);
                return true;
            }
        }
    }
    false
}

/// `GET /api/org`: the organization of this workspace, read off the workspace
/// record.
///
/// A workspace is one Grafana server with one organization
/// ([`WORKSPACE_ORG_ID`]), and the organization is the workspace — so its name is
/// the workspace's `name`, or the workspace id when the create named none
/// (`CreateWorkspaceRequest$workspaceName` is optional).
fn org_response(workspace: &Workspace) -> Response {
    let name = workspace.name.clone().unwrap_or_else(|| workspace.id.clone());
    let body = ::serde_json::json!({
        "id": WORKSPACE_ORG_ID,
        "name": name,
    });
    Response {
        status: 200,
        body: body.to_string().into_bytes(),
    }
}

/// One `{"message": ...}` answer, the envelope the Grafana HTTP API states a
/// refusal in.
fn json(status: u16, message: &str) -> Response {
    let body = ::serde_json::json!({ "message": message });
    Response {
        status,
        body: body.to_string().into_bytes(),
    }
}
