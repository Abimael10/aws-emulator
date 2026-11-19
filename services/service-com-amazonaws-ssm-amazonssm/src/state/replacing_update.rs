// SPDX-License-Identifier: CC0-1.0

//! What an update of one resource leaves behind, for the operations of this service that
//! take a `Replace` flag beside the members they modify.
//!
//! Those updates all follow ONE rule, worded the same way on each of them: "if a parameter
//! is null, then the corresponding field isn't modified", and "if True, then all fields that
//! are required by the <register/create> operation are also required for this API request.
//! Optional fields that aren't specified are set to null"
//! (`UpdateMaintenanceWindowTargetRequest$Replace`,
//! `UpdatePatchBaselineRequest$Replace`). So an update is a PATCH of what is stored by
//! default and a REPLACEMENT of it under the flag, and a replacing update that leaves out a
//! member the resource's own create requires is missing a member it was required to carry.
//!
//! This module is the one place that rule is written, so every operation holding to it
//! applies it the same way rather than each deciding what an omitted member means.

/// What one member of the updated resource carries.
///
/// A member the request supplied is taken as given, and a member it left out keeps `stored`.
/// Under `Replace`, a member the request left out carries `cleared` instead — the value a
/// create that named that member nothing would have persisted, which is how a read reports a
/// member nothing ever set.
pub fn updated_member<T>(supplied: Option<T>, stored: T, cleared: T, replace: bool) -> T {
    match supplied {
        Some(value) => value,
        None if replace => cleared,
        None => stored,
    }
}

/// [`updated_member`] for a member a read reports only when a write set it.
///
/// Such a member's own type is already the optional one, so the request supplies it exactly
/// when it carries a value: a request that leaves it out leaves the stored value alone, and
/// a replacing update clears it to the nothing a create that named it carried.
pub fn updated_optional_member<T>(
    supplied: Option<T>,
    stored: Option<T>,
    replace: bool,
) -> Option<T> {
    updated_member(supplied.map(Some), stored, None, replace)
}

/// How an update that asks to REPLACE a resource without carrying `member` is refused.
///
/// Under the flag the members the resource's own create requires are required here too, so
/// such a request is missing a member it was required to carry, and is stated as exactly
/// that: the clause the boundary walk states over a member that must not be null, in the
/// envelope this service's refusals carry, neither of which is spelled here.
///
/// `member` is the modelled member's name in the spelling this service's refusals carry it
/// in — its modelled name with a lowercase first letter, the way `documentVersion` names
/// `DocumentVersion`.
#[must_use]
pub fn replace_requires_member_message(member: &str) -> String {
    replace_requires_members_message(std::slice::from_ref(&member))
}

/// [`replace_requires_member_message`] over EVERY member such a request left out.
///
/// A create may require several members, and the boundary walk states one clause per member
/// that must not be null inside a single envelope rather than stopping at the first — so a
/// replacing update missing several of them is refused the same way, naming each in the
/// order the model declares them.
#[must_use]
pub fn replace_requires_members_message(members: &[&str]) -> String {
    let clauses: Vec<String> = members.iter().copied().map(missing_member_clause).collect();
    // The envelope answers `None` only for NO clause at all, and a caller refuses a request
    // only over a member it found missing, so the fallback is unreachable; it keeps the
    // clauses themselves rather than panicking on an envelope that could not be worded.
    ::service_common::validation_message::envelope_over(&clauses)
        .unwrap_or_else(|| clauses.concat())
}

/// The clause the boundary walk states over a member that must not be null, worded for
/// `member`.
fn missing_member_clause(member: &str) -> String {
    format!(
        "Value null at '{member}' failed to satisfy constraint: {}",
        ::service_common::validation_message::requirement(
            ::aws_smithy_schema_server::constraints::ViolationKind::MissingRequiredMember
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A member the request supplied is taken as given, one it left out keeps what is stored,
    /// and one it left out of a REPLACING update is cleared.
    #[test]
    fn an_omitted_member_is_kept_by_a_patch_and_cleared_by_a_replacement() {
        // (case, what the request supplied, whether it replaces, what the member carries)
        let rows: Vec<(&str, Option<&str>, bool, &str)> = vec![
            ("supplied by a patch", Some("supplied"), false, "supplied"),
            ("supplied by a replacement", Some("supplied"), true, "supplied"),
            ("omitted by a patch", None, false, "stored"),
            ("omitted by a replacement", None, true, "cleared"),
        ];
        for (case, supplied, replace, expected) in rows {
            let carried = updated_member(supplied, "stored", "cleared", replace);
            assert_eq!(
                carried, expected,
                "case {case:?}: expected {expected:?}, got {carried:?}"
            );
            let optional = updated_optional_member(supplied, Some("stored"), replace);
            let expected_optional = match (supplied, replace) {
                (Some(value), _) => Some(value),
                (None, true) => None,
                (None, false) => Some("stored"),
            };
            assert_eq!(
                optional, expected_optional,
                "case {case:?}: expected the optional member to carry \
                 {expected_optional:?}, got {optional:?}"
            );
        }
    }

    /// The refusal of a replacing update that leaves out a required member names that member.
    #[test]
    fn the_refusal_of_a_replacement_names_the_member_it_wanted() {
        for member in ["targets", "name"] {
            let message = replace_requires_member_message(member);
            assert!(
                message.contains(&format!("'{member}'")),
                "case {member:?}: expected the refusal to name the member, got {message:?}"
            );
        }
    }
}
