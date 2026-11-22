// SPDX-License-Identifier: CC0-1.0

//! The text a `ConflictException` of this service carries when a request conflicts with
//! the state the store already holds, as ONE template per resource kind and conflict
//! class.
//!
//! The model gives `ConflictException` a required `message` ("The operation could not be
//! completed because of a conflict with the current state of the resource.") and fixes no
//! text for it, so the wording is the service's own and is recorded behavior. What the
//! recordings show is that each conflict CLASS has its own sentence, and that the sentence
//! carries a value of the request or of the store only where its text has a place for one:
//!
//! * a resource whose name is its identity in the ACCOUNT AND REGION is named: the kind,
//!   the fact that one already holds the name, and then that name — the same shape the
//!   miss on the same kind states ([`super::resource_not_found`]), so a caller reads one
//!   resource named one way whether it was asked for and absent or asked for and already
//!   there;
//! * a resource whose name identifies it only WITHIN A SPACE is told the kind and that the
//!   space already holds one under that name, with neither the name nor the space id in
//!   it: the conflict is with a collection the caller addressed rather than with a
//!   resource it named, so nothing it submitted is reflected back;
//! * a delete refused because the resource still holds the children of another kind is
//!   told the kind, the identifier it addressed, HOW MANY children the store holds for it
//!   and what to remove first — the count is read from state at the moment of the
//!   refusal, so a caller can report how much is left to clear.
//!
//! One template per resource kind and class, reached from here by every operation that
//! applies it: an idempotent-create wrapper that branches on the text sees the same
//! sentence whichever operation reported the conflict.

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

/// The whole message a start that names a session the caller already holds open earns.
///
/// Recorded behavior, and the one place this service's conflict wording does NOT follow
/// the kind-and-identifier template above: wholly FIXED text, a sentence with a trailing
/// period, naming the member that collided rather than interpolating the name the request
/// carried — so a refused start discloses nothing of what the caller already holds beyond
/// that the name is taken.
pub const SESSION_NAME_ALREADY_EXISTS_MESSAGE: &str = "A session with this name already exists.";

/// How a name already taken inside a SPACE is worded: the kind the name would have
/// identified, and that the space the request addressed already holds one under it.
///
/// Neither the name nor the `spaceId` is a variable of this sentence — the kind is the
/// only thing that differs between two of them — so the refusal reads the same whatever
/// the caller submitted, and a client that surfaces it to a user discloses nothing of the
/// request.
macro_rules! name_taken_in_space_message {
    ($resource:literal) => {
        concat!($resource, " with that name already exists in this space")
    };
}

/// The message a create or a rename that names an access profile the addressed space
/// already holds earns.
pub const ACCESS_PROFILE_NAME_TAKEN: &str = name_taken_in_space_message!("An access profile");

/// The message a create or a rename that names an access grant the addressed space
/// already holds earns.
pub const ACCESS_GRANT_NAME_TAKEN: &str = name_taken_in_space_message!("An access grant");

/// The message a `DeleteDomain` of a domain that still holds spaces earns: the kind and
/// the identifier the request addressed, how many spaces the store holds in that domain,
/// and that all of them go first.
///
/// The rule is the model's own doc for the operation, "A domain cannot be deleted while it
/// contains spaces"; the sentence is recorded behavior. `spaces` is read from the space
/// records when the refusal is stated rather than remembered, so the number is the one a
/// caller would see by listing that domain's spaces, and the kinds are named as this
/// service names them in a `ConflictException`'s `resourceType`.
pub fn domain_holds_spaces_message(domain_id: &str, spaces: usize) -> String {
    format!(
        "Cannot delete Domain {domain_id} because it has {spaces} associated Spaces. Delete \
         all Spaces first."
    )
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

    #[test]
    fn a_name_taken_inside_a_space_states_the_kind_and_nothing_submitted() {
        // The expected sentences are the recorded wording for the kind; the submitted
        // values below span the name classes the models admit for the two kinds —
        // `AccessProfileName` and `AccessGrantName` — and the `SpaceId` UUID pattern,
        // because neither a name nor a space id is a variable of the sentence.
        for (case, message, expected) in [
            (
                "an access profile",
                ACCESS_PROFILE_NAME_TAKEN,
                "An access profile with that name already exists in this space",
            ),
            (
                "an access grant",
                ACCESS_GRANT_NAME_TAKEN,
                "An access grant with that name already exists in this space",
            ),
        ] {
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            for submitted in [
                "operations",
                "profile b98f45b7d422",
                &"n".repeat(256),
                "00000000-0000-4000-8000-000000000000",
                "e7464b35-e94a-4189-b428-85a318704bf4",
            ] {
                assert!(
                    !message.contains(submitted),
                    "case {case:?}: the message carries the submitted {submitted:?}, got \
                     {message:?}"
                );
            }
            assert!(
                !message.ends_with('.'),
                "case {case:?}: the message carries no trailing period, got {message:?}"
            );
        }
    }

    #[test]
    fn a_delete_blocked_by_the_spaces_a_domain_holds_states_how_many_there_are() {
        // Rows span the counts a store can hold under one domain: the one space that is
        // enough to block the delete, and more than one, because the count is read from
        // state rather than fixed. The domain ids span `DomainId`'s `^d-[0-9a-z]{1,25}$`.
        for (case, actual, expected) in [
            (
                "a domain holding one space",
                domain_holds_spaces_message("d-1naa6qgdhi2sun9oyzl7cpkfk", 1),
                "Cannot delete Domain d-1naa6qgdhi2sun9oyzl7cpkfk because it has 1 associated \
                 Spaces. Delete all Spaces first.",
            ),
            (
                "a domain holding several spaces",
                domain_holds_spaces_message("d-eyk2q9grxo064qkn84wtzszw8", 7),
                "Cannot delete Domain d-eyk2q9grxo064qkn84wtzszw8 because it has 7 associated \
                 Spaces. Delete all Spaces first.",
            ),
            (
                "the shortest domain id the model admits, at a three-digit count",
                domain_holds_spaces_message("d-0", 123),
                "Cannot delete Domain d-0 because it has 123 associated Spaces. Delete all \
                 Spaces first.",
            ),
        ] {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
