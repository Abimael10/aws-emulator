// SPDX-License-Identifier: CC0-1.0

//! The text a `ResourceNotFoundException` of this service carries, as ONE template per
//! resource kind.
//!
//! The model gives `ResourceNotFoundException` a required `message` ("The specified
//! resource does not exist.") and fixes no text for it, so the wording is the service's
//! own and is recorded behavior. What it records is a two-part sentence: the kind of
//! resource that was looked for, then the identifier the request addressed it by —
//! `<Resource> not found: <identifier>`. The identifier is appended after the colon
//! exactly as the request wrote it, unquoted; nothing else follows it, there is no
//! trailing period, and the identifier of a CONTAINING resource the request also carried
//! is not named, because a miss is about the resource that is missing.
//!
//! One template per resource kind, reached from here by every operation that addresses
//! that kind: a caller matching the prefix to tell this miss from any other 404 on the
//! operation sees the same sentence whichever operation reported it, so a read, a patch
//! and a delete of the same absent resource cannot word it three ways.

/// How a missing resource's kind and the identifier it was addressed by are joined.
///
/// Both parts are as the request and the schema give them, so the only fixed text is
/// this.
fn not_found_message(resource: &str, identifier: &str) -> String {
    format!("{resource} not found: {identifier}")
}

/// How a miss names a dashboard.
///
/// A dashboard is addressed by a `dashboardId` inside a space, and the miss names the
/// resource as the dashboard it is rather than by its modelled shape name.
pub const DASHBOARD_NOT_FOUND_RESOURCE: &str = "Dashboard";

/// How a miss names a view.
pub const VIEW_NOT_FOUND_RESOURCE: &str = "View";

/// The message a request for a dashboard the store holds nothing under earns, whichever
/// operation addressed it: the dashboard id the request carried, and no space id.
pub fn dashboard_not_found_message(dashboard_id: &str) -> String {
    not_found_message(DASHBOARD_NOT_FOUND_RESOURCE, dashboard_id)
}

/// The message a request for a view the store holds nothing under earns, whichever
/// operation addressed it: the view name the request carried, which is a view's identity.
pub fn view_not_found_message(name: &str) -> String {
    not_found_message(VIEW_NOT_FOUND_RESOURCE, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_miss_names_the_resource_kind_then_the_identifier_the_request_carried() {
        // Rows span the identifier forms each kind is addressed by — a dashboard by the
        // 36-character `DashboardId` pattern, a view by the whole `ViewName` class
        // (`@length(6..256)`, `^view\.[a-z0-9][a-z0-9_-]{0,250}$`) — because the template
        // takes the identifier verbatim rather than reformatting it.
        for (case, actual, expected) in [
            (
                "a dashboard id of nil bytes",
                dashboard_not_found_message("00000000-0000-0000-0000-000000000000"),
                "Dashboard not found: 00000000-0000-0000-0000-000000000000",
            ),
            (
                "a dashboard id of mixed digits",
                dashboard_not_found_message("8a4f1b3c-0e2d-4a5b-9c7d-1f2e3a4b5c6d"),
                "Dashboard not found: 8a4f1b3c-0e2d-4a5b-9c7d-1f2e3a4b5c6d",
            ),
            (
                "the shortest view name the model admits",
                view_not_found_message("view.a"),
                "View not found: view.a",
            ),
            (
                "a view name carrying the punctuation its pattern admits",
                view_not_found_message("view.0_errors-by-service"),
                "View not found: view.0_errors-by-service",
            ),
            (
                "the longest view name the model admits",
                view_not_found_message(&format!("view.{}", "v".repeat(251))),
                &format!("View not found: view.{}", "v".repeat(251)),
            ),
        ] {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
