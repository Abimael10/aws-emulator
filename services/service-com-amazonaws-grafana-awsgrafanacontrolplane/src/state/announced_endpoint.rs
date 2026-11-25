// SPDX-License-Identifier: CC0-1.0

//! WHERE a workspace's Grafana console and native Grafana HTTP API are ANNOUNCED: the
//! value `WorkspaceDescription$endpoint` / `WorkspaceSummary$endpoint` carries, and the
//! addressing [`crate::service::workspace_api`] reads back off a request.
//!
//! # The rule this module implements
//!
//! `endpoint` is "The URL that users can use to access the Grafana console in the
//! workspace" (`smithy_query(query="shape", shape="WorkspaceDescription")`), so it is not a
//! label a client displays but an ADDRESS it dials: it splices it into a request of its own
//! (`https://<endpoint>/api/org`) to reach that workspace's Grafana HTTP API. The only
//! value that can carry is an address THIS emulator answers on — the `host[:port]` a
//! request reached it at ([`announced_authority`]), with the workspace's own identity in
//! the PATH ([`workspace_endpoint_path`]), because this process serves its whole surface at
//! one endpoint and the path is where a per-resource address can still be distinct.
//!
//! A name in a zone Amazon Web Services publishes its own endpoints in resolves nowhere
//! near this process, or at genuine infrastructure: a client handed one leaves the emulated
//! environment carrying the credentials it holds and reaches no workspace at all. So no
//! such name is announced, however the real service spells its own.
//!
//! The value carries no scheme, exactly as the one the real service announces carries none,
//! so the splice a client already performs on it keeps working unchanged.
//!
//! Nothing here opens a connection or resolves a name: every function reads request parts
//! or a string and returns a string.

use ::service_common::data_plane_addressing as addressing;
use ::smithy_server_runtime::OperationContext;

/// The path segment naming THIS service's data plane, under the reserved namespace every
/// plane this emulator serves on its own endpoint sits beneath
/// ([`addressing::EMULATOR_PATH_NAMESPACE`]): this service's own `arnNamespace`, so no other
/// service's plane can claim the same path.
pub const ENDPOINT_PLANE_SEGMENT: &str = crate::state::GRAFANA_ARN_NAMESPACE;

/// The `host[:port]` this emulator is reached at, for the request `ctx` is answering.
///
/// The three readings are the shared ones, taken in the order every data plane served at
/// the front door announces an address in:
///
/// * the EMULATOR'S OWN endpoint this request addressed, port included
///   ([`addressing::reached_emulator_endpoint_of_parts`]) — a client configured with one
///   local endpoint resolves nothing else, so it is handed back the very address it is
///   already talking to;
/// * the address this process is served at ([`addressing::served_endpoint_authority`]) for
///   a request that addressed a host of the real service's instead, so an in-process call
///   of another service announces an address that service's caller can reach too;
/// * the base an emulator serving no port at all reports
///   ([`addressing::emulator_endpoint_authority`]) — a host nothing resolves, which is the
///   truth about such an emulator rather than a name of the real service's.
#[must_use]
pub fn announced_authority(ctx: &OperationContext<'_>) -> String {
    addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| addressing::served_endpoint_authority(ctx.endpoint().as_str()))
        .unwrap_or_else(|| addressing::emulator_endpoint_authority(ctx))
}

/// The endpoint announced for the workspace `workspace_id` on an emulator reached at
/// `authority`: that authority and the workspace's own path beneath it.
///
/// It is what every projection of a workspace reports, so two workspaces never share one
/// address and the address a client reads is one it can dial.
#[must_use]
pub fn workspace_endpoint(authority: &str, workspace_id: &str) -> String {
    format!(
        "{}{}",
        authority.trim_end_matches('/'),
        workspace_endpoint_path(workspace_id)
    )
}

/// The path a workspace's Grafana HTTP API is served under on this emulator's own endpoint:
/// `/_aws/grafana/<workspaceId>`.
///
/// It carries what the real service's published host carries — WHICH workspace is addressed
/// — moved out of a name this process cannot answer on and into the path.
#[must_use]
pub fn workspace_endpoint_path(workspace_id: &str) -> String {
    format!(
        "/{}/{ENDPOINT_PLANE_SEGMENT}/{workspace_id}",
        addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// The workspace an announced endpoint path addresses, and the Grafana route beneath it —
/// and `None` for a path that is not an endpoint this service announces.
///
/// The inverse of [`workspace_endpoint_path`], and the reader of the addressing this service
/// publishes: the first label of the real service's host travels in a path segment here, so
/// the segment must satisfy `WorkspaceId` ([`crate::state::is_workspace_id`]) exactly as
/// that label does. A request that named no route under the endpoint addresses the Grafana
/// API's own root (`/`).
#[must_use]
pub fn workspace_at_announced_path(path: &str) -> Option<(String, String)> {
    let (namespace, rest) = addressing::first_segment(path)?;
    if namespace != addressing::EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = addressing::first_segment(rest)?;
    if plane != ENDPOINT_PLANE_SEGMENT {
        return None;
    }
    let (workspace_id, route) = addressing::first_segment(rest)?;
    crate::state::is_workspace_id(workspace_id)
        .then(|| (workspace_id.to_string(), route.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an announced endpoint is an authority this emulator answers on plus the
    /// addressed workspace's own path, and it reads back as exactly that workspace.
    ///
    /// The rows span the forms an emulator's address takes: loopback by address with a
    /// port, loopback by name, a bracketed IPv6 literal, a routable address, a name with no
    /// port, and the base of an emulator served on no port at all.
    #[test]
    fn an_announced_endpoint_is_an_address_of_this_emulator_naming_the_workspace() {
        let rows: &[(&str, &str, &str)] = &[
            ("loopback with a port", "127.0.0.1:9297", "g-0123456789"),
            ("loopback by name", "localhost:39355", "g-abcdef0123"),
            ("a bracketed IPv6 literal", "[::1]:8443", "g-00000000ff"),
            ("a routable address", "198.51.100.7:8443", "g-deadbeef01"),
            ("a name with no port", "emulator-host", "g-0000000001"),
            ("an emulator served nowhere", "emulator.invalid", "g-1234567890"),
        ];
        for (case, authority, workspace_id) in rows {
            let announced = workspace_endpoint(authority, workspace_id);
            let want = format!("{authority}/_aws/grafana/{workspace_id}");
            assert_eq!(
                announced, want,
                "case {case:?}: expected {want:?}, got {announced:?}"
            );
            let read_back = workspace_at_announced_path(&format!(
                "{}/api/org",
                workspace_endpoint_path(workspace_id)
            ));
            assert_eq!(
                read_back,
                Some(((*workspace_id).to_string(), "/api/org".to_string())),
                "case {case:?}: the announced path must read back as its workspace and \
                 route, got {read_back:?}"
            );
        }
    }

    /// RULE: only a path under this service's own plane segment that names a `WorkspaceId`
    /// is an announced endpoint of this service; everything else is claimed by nothing here.
    #[test]
    fn a_path_that_is_no_announced_endpoint_names_no_workspace() {
        /// One row: the case, the path, and the workspace and route it names.
        type Row = (&'static str, &'static str, Option<(&'static str, &'static str)>);
        let rows: &[Row] = &[
            (
                "the endpoint itself",
                "/_aws/grafana/g-0123456789",
                Some(("g-0123456789", "/")),
            ),
            (
                "a route beneath it",
                "/_aws/grafana/g-0123456789/api/org",
                Some(("g-0123456789", "/api/org")),
            ),
            (
                "a deeper route beneath it",
                "/_aws/grafana/g-0123456789/api/org/users",
                Some(("g-0123456789", "/api/org/users")),
            ),
            ("a control-plane route", "/workspaces/g-0123456789", None),
            ("another service's plane", "/_aws/athena/g-0123456789", None),
            ("no namespace segment", "/grafana/g-0123456789/api/org", None),
            (
                "an id outside WorkspaceId",
                "/_aws/grafana/not-a-workspace/api/org",
                None,
            ),
            (
                "an id in upper case, which WorkspaceId admits nowhere",
                "/_aws/grafana/G-0123456789/api/org",
                None,
            ),
            ("no workspace at all", "/_aws/grafana", None),
            ("a relative path", "_aws/grafana/g-0123456789", None),
        ];
        for (case, path, want) in rows {
            let read = workspace_at_announced_path(path);
            let expected =
                want.map(|(id, route)| (id.to_string(), route.to_string()));
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?} for {path:?}, got {read:?}"
            );
        }
    }
}
