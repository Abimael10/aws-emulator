// SPDX-License-Identifier: CC0-1.0

//! A list member a request CARRIES but states no entry in.
//!
//! A list the request leaves out and a list it sends empty are two different requests:
//! the first asks for nothing, while the second declares a list OF nothing — and a member
//! whose whole content is its entries cannot be satisfied by an empty one, so it is
//! refused rather than read as absent. The model does not state the rule for the index
//! lists a table declaration carries (`smithy_query(query="operation",
//! operation="CreateTable")` reports `GlobalSecondaryIndexes` and `LocalSecondaryIndexes`
//! with no `@length` minimum), so the service is what enforces it, with one fixed
//! parameter-value sentence naming the member ([`empty_list_message`]). The tagging
//! surface's required tag lists are held to the same rule in their own vocabulary
//! ([`no_tag_entries_message`]).
//!
//! It lives beside [`super`] rather than inside it because it is about the shape of the
//! REQUEST rather than about the table and item store; `super` re-exports it, so every
//! caller still names it `crate::state::…`.

use super::RequestError;

/// AWS's fixed text for a list member the request carries with no entries at all: the
/// sentence is fixed throughout and the member's own name, as the model spells it on the
/// wire, is the only substituted part — the fact is about the member rather than about the
/// resource the request addressed.
pub fn empty_list_message(member: &str) -> String {
    format!("One or more parameter values were invalid: List of {member} is empty")
}

/// AWS's text for a REQUIRED TAG list the request carries with no entries at all, which
/// the tagging surface words in its own vocabulary: the sentence is fixed throughout and
/// the only substituted part is what the list's entries ARE — a tag for the list a tag
/// write states, a tag key for the list a tag removal states — because the fact is about
/// the entry the request stated none of and not about the resource it addressed.
///
/// The model states no minimum for either list (`smithy_query(query="operation",
/// operation="TagResource,UntagResource")` reports `Tags` and `TagKeys` as `@required`
/// with no `@length`), so the service is what enforces it.
pub fn no_tag_entries_message(entry: &str) -> String {
    format!("Atleast one {entry} needs to be provided as Input.")
}

/// What the entries of a tag WRITE's list are, as the sentence above names them.
pub const TAG_LIST_ENTRY: &str = "Tag";

/// What the entries of a tag REMOVAL's list are, as the sentence above names them.
pub const TAG_KEY_LIST_ENTRY: &str = "Tag Key";

/// Refuse a required tag list the request states no `entry` of.
///
/// An empty list is an INPUT failure, so it is refused ahead of the selector the request
/// addressed a resource by: a request that states no tag cannot be served whatever that
/// selector names — a resource it does not resolve to at all included — and a caller told
/// its ARN is wrong would fix the wrong member.
pub fn check_tag_entries_stated(entry: &str, entries: usize) -> Result<(), RequestError> {
    if entries == 0 {
        return Err(RequestError::Validation(no_tag_entries_message(entry)));
    }
    Ok(())
}

/// Refuse the first member of `members` the request carries with no entries.
///
/// Each entry pairs a member's wire name with the number of entries the request stated for
/// it, and `None` is a member the request leaves out — which asks for nothing and is no
/// defect. The caller lists the members in the order it reads them, which is the order a
/// refusal names them in.
pub fn check_lists_not_empty<'m>(
    members: impl IntoIterator<Item = (&'m str, Option<usize>)>,
) -> Result<(), RequestError> {
    for (member, entries) in members {
        if entries == Some(0) {
            return Err(RequestError::Validation(empty_list_message(member)));
        }
    }
    Ok(())
}
