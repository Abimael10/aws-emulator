// SPDX-License-Identifier: CC0-1.0

//! The text a `ConflictException` of this service carries when the identity a create
//! asks for is already taken, as ONE template per resource kind.
//!
//! The model gives `ConflictException` a required `message` ("The operation could not be
//! completed because of a conflict with the current state of the resource.") and fixes no
//! text for it, so the wording is the service's own and is recorded behavior. What the
//! recordings show for a resource whose NAME is its identity is the kind, the fact that
//! one already holds the name, and then that name: the same shape the miss on the same
//! kind states ([`super::resource_not_found`]), so a caller reads one resource named one
//! way whether it was asked for and absent or asked for and already there.
//!
//! One template per resource kind, reached from here by every operation that mints that
//! kind: an idempotent-create wrapper that branches on the text sees the same sentence
//! whichever operation reported the conflict.

/// How a taken identity's kind and the identifier it is held under are joined.
///
/// The identifier is appended after the colon exactly as the request wrote it, unquoted;
/// nothing else follows it and there is no trailing period, so the only fixed text is
/// this.
fn already_exists_message(resource: &str, identifier: &str) -> String {
    format!("{resource} already exists: {identifier}")
}

/// The message a create that names a view the store already holds earns: the view name
/// the request carried, which is a view's identity.
///
/// The kind is named as the miss on a view names it, so the two refusals on one view name
/// cannot call it two things.
pub fn view_already_exists_message(name: &str) -> String {
    already_exists_message(super::resource_not_found::VIEW_NOT_FOUND_RESOURCE, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_view_name_is_appended_verbatim_after_the_kind() {
        // Rows span the `ViewName` class the model admits (`@length(6..256)`,
        // `^view\.[a-z0-9][a-z0-9_-]{0,250}$`), because the template takes the name
        // verbatim rather than reformatting it.
        for (case, actual, expected) in [
            (
                "the shortest view name the model admits",
                view_already_exists_message("view.a"),
                "View already exists: view.a",
            ),
            (
                "a view name carrying the punctuation its pattern admits",
                view_already_exists_message("view.0_errors-by-service"),
                "View already exists: view.0_errors-by-service",
            ),
            (
                "the longest view name the model admits",
                view_already_exists_message(&format!("view.{}", "v".repeat(251))),
                &format!("View already exists: view.{}", "v".repeat(251)),
            ),
        ] {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
