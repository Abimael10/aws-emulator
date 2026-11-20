// SPDX-License-Identifier: CC0-1.0

//! The text a `ResourceNotFoundException` of this service carries, as ONE template per
//! resource kind.
//!
//! The model gives `ResourceNotFoundException` a required `message` ("The specified
//! resource does not exist.") and fixes no text for it, so the wording is the service's
//! own and is recorded behavior. What the recordings show is that the service words a
//! miss PER RESOURCE KIND rather than from one sentence pattern over every kind: each
//! kind has its own fixed text, and that text carries an identifier only where it has a
//! place for one. Most kinds name none, so the identifier the request addressed the
//! resource by is withheld rather than interpolated, and the identifier of a CONTAINING
//! resource the request also carried appears only where the kind's own text names it.
//!
//! One template per resource kind, reached from here by every operation that addresses
//! that kind: a caller matching the text to tell this miss from any other 404 on the
//! operation sees the same sentence whichever operation reported it, so a read, a patch,
//! a delete and a create that names the kind as its parent cannot word one absent
//! resource four ways.

/// How a missing resource's kind and the identifier it was addressed by are joined, for
/// the kinds whose text has a place for that identifier.
///
/// The identifier is appended after the colon exactly as the request wrote it, unquoted;
/// nothing else follows it and there is no trailing period, so the only fixed text is
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

/// The message a request for a domain the store holds nothing under earns, whichever
/// operation addressed it — a read, an update, or a call that names the domain as the
/// parent of what it addresses.
///
/// Recorded behavior: wholly FIXED text with no variable part. The `domainId` the request
/// carried does not appear in it, so a miss discloses nothing of what was asked for
/// beyond that it is not there.
pub const DOMAIN_NOT_FOUND_MESSAGE: &str = "Domain not found";

/// The message a request for a space the store holds nothing under earns, whichever
/// operation addressed it.
///
/// No recording of this service words a missing space, so this follows the form of the
/// kinds that ARE recorded with wholly fixed text — a domain, an access profile and an
/// access grant — and withholds the `spaceId` exactly as those do, rather than inventing
/// an interpolating sentence of its own.
pub const SPACE_NOT_FOUND_MESSAGE: &str = "Space not found";

/// The message a request for an access profile the store holds nothing under earns,
/// whichever operation addressed it, and whichever space the request scoped it to.
///
/// Recorded behavior: wholly FIXED text, in sentence case, naming neither the profile nor
/// the space the request carried.
pub const ACCESS_PROFILE_NOT_FOUND_MESSAGE: &str = "Access profile not found";

/// The message a request for an access grant the store holds nothing under earns,
/// whichever operation addressed it.
///
/// Recorded behavior: wholly FIXED text naming the resource by its modelled CamelCase
/// shape name, `AccessGrant`, and carrying no identifier.
pub const ACCESS_GRANT_NOT_FOUND_MESSAGE: &str = "AccessGrant not found";

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

/// The message a request for an alert the store holds nothing under earns.
///
/// Recorded behavior, and the one kind whose text names the containing resource as well:
/// the fixed words around TWO variables, the `alertId` and the `spaceId` the request
/// addressed the alert by, each single-quoted, with no trailing period. An alert is
/// identified by both — the same id under another space is not this request's alert — so
/// the message states the pair it looked for.
pub fn alert_not_found_message(alert_id: &str, space_id: &str) -> String {
    format!("No alert '{alert_id}' in space '{space_id}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_whose_text_names_no_identifier_carries_nothing_of_the_request() {
        // Rows span the identifier forms each of these kinds is addressed by — a domain by
        // `DomainId` (`^d-[0-9a-z]{1,25}$`), a space and a grant by the UUID `SpaceId` /
        // `AccessGrantId` patterns, a profile by its `profileId` — because the message is
        // the same fixed text whatever the request carried.
        for (case, message, identifier) in [
            ("a domain", DOMAIN_NOT_FOUND_MESSAGE, "d-0123456789abcdefghijklmno"),
            ("a space", SPACE_NOT_FOUND_MESSAGE, "00000000-0000-4000-8000-000000000000"),
            (
                "an access profile",
                ACCESS_PROFILE_NOT_FOUND_MESSAGE,
                "8a4f1b3c-0e2d-4a5b-9c7d-1f2e3a4b5c6d",
            ),
            (
                "an access grant",
                ACCESS_GRANT_NOT_FOUND_MESSAGE,
                "496d9e19-c704-4000-8000-000000000000",
            ),
        ] {
            assert!(
                !message.contains(identifier),
                "case {case:?}: the message must not carry the identifier the request \
                 addressed the resource by, got {message:?}"
            );
            assert!(
                !message.ends_with('.'),
                "case {case:?}: the message carries no trailing period, got {message:?}"
            );
        }
    }

    #[test]
    fn a_kind_whose_text_has_a_place_for_the_identifier_appends_it_verbatim() {
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
            (
                "an alert and the space it was looked for in",
                alert_not_found_message(
                    "e70d59a037802d0f10853b79dc813857",
                    "00000000-0000-4000-8000-c5a4f0a9ee2f",
                ),
                "No alert 'e70d59a037802d0f10853b79dc813857' in space \
                 '00000000-0000-4000-8000-c5a4f0a9ee2f'",
            ),
            (
                "an alert id of another form, in another space",
                alert_not_found_message("0f10853b79dc813857e70d59a037802d", "space-two"),
                "No alert '0f10853b79dc813857e70d59a037802d' in space 'space-two'",
            ),
        ] {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
