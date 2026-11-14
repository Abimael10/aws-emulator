// SPDX-License-Identifier: CC0-1.0

//! The Studio/notebook plane: how the backend fronting it refuses a request the
//! framework would otherwise refuse in the generic constraint envelope.
//!
//! EMR Studio and EMR Notebooks are not served by the cluster plane. They are fronted by
//! their own backend, and EVERY refusal that backend raises is worded the same way: its
//! own sentence for what it rejected, followed by the parenthesised service detail
//! naming the backend, the status, the error code and the response's own request id
//! ([`crate::state::editors_refusal_message`]). The handlers of this plane already answer
//! that way — an unknown Studio, an unknown notebook execution, a network member or a
//! service role that resolves to nothing — so a value outside a modelled ENUM of one of
//! those operations' own input shapes, which the framework reaches BEFORE any handler
//! runs, is worded here rather than left to the generic request validator's envelope.
//!
//! What the backend says about such a value is the member's own sentence
//! ([`enum_value_sentence`]), and never the set of values the enum declares: the
//! framework's envelope enumerates that set, which this plane does not disclose.

use smithy_server_runtime::Violation;

use crate::ops::types::InvalidRequestException;

/// The operations EMR serves through the Studio/notebook backend: the Studio resource and
/// its session mappings, and the notebook executions a Studio's editors run.
///
/// Read off this service's own surface (the crate root's `OPERATIONS.md`, generated from
/// the Smithy model): these are the operations whose refusals carry that backend's
/// service detail. The cluster plane is everything else, and words its refusals itself.
///
/// Sorted, which [`on_editors_plane`] relies on for `binary_search` and
/// `the_editors_plane_operations_are_sorted` pins.
pub(super) const EDITORS_PLANE: &[&str] = &[
    "CreateStudio",
    "CreateStudioSessionMapping",
    "DeleteStudio",
    "DeleteStudioSessionMapping",
    "DescribeNotebookExecution",
    "DescribeStudio",
    "GetStudioSessionMapping",
    "ListNotebookExecutions",
    "ListStudioSessionMappings",
    "ListStudios",
    "StartNotebookExecution",
    "StopNotebookExecution",
    "UpdateStudio",
    "UpdateStudioSessionMapping",
];

/// Whether `operation` is served by the Studio/notebook backend.
pub(super) fn on_editors_plane(operation: &str) -> bool {
    EDITORS_PLANE.binary_search(&operation).is_ok()
}

/// The member a notebook-execution listing filters by execution status under, as a
/// validator member path spells it.
const NOTEBOOK_EXECUTION_STATUS_MEMBER: &str = "status";

/// The sentence the backend states a value outside a modelled enum of `member` with.
///
/// Recorded behavior for the text, and it is the MEMBER that decides it:
///
/// * the execution status a notebook-execution listing filters by is stated as an invalid
///   execution status, with the rejected value — the one place this plane echoes the value
///   a request carried;
/// * every other enum-valued member is stated by naming the member, as the model spells
///   it, and that it is not valid — the same template this plane's network members are
///   refused by ([`crate::state::invalid_network_member_message`]), because the template
///   is the backend's rather than any one member's, so a member recorded later confirms
///   its own instance.
///
/// `member` arrives as a validator member path spells it (lower-camel), and the sentence
/// states it as the model spells it, which is that path with its first letter raised.
pub(super) fn enum_value_sentence(member: &str, value: Option<&str>) -> String {
    if member == NOTEBOOK_EXECUTION_STATUS_MEMBER {
        return format!(
            "Invalid Execution Status: {}",
            value.unwrap_or(crate::state::ABSENT_VALUE)
        );
    }
    format!("{} is not valid.", super::opening(member))
}

/// The refusal this plane answers values outside a modelled enum of the served
/// operation's own input shape with: the member's sentence inside the backend's service
/// detail.
///
/// The shape carries no `ErrorCode`, as every other refusal this backend raises carries
/// none: the error code a caller branches on is the one the wrapper already states.
/// `None` when no violation of this refusal is on an enum-valued member of the
/// operation's own input shape, which leaves that refusal exactly where it was.
pub(super) fn enum_value_refusal(violations: &[Violation]) -> Option<InvalidRequestException> {
    let first = *super::input_member_enum_violations(violations).first()?;
    let member = first.path.split('.').next()?;
    Some(InvalidRequestException {
        error_code: None,
        message: Some(crate::state::editors_refusal_message(&enum_value_sentence(
            member,
            first.value.as_deref(),
        ))),
    })
}

#[cfg(test)]
mod tests {
    use super::{enum_value_sentence, on_editors_plane, EDITORS_PLANE};

    /// The table is searched with `binary_search`, so it must be sorted and hold no
    /// duplicate.
    #[test]
    fn the_editors_plane_operations_are_sorted() {
        assert!(
            EDITORS_PLANE.windows(2).all(|pair| pair[0] < pair[1]),
            "case {EDITORS_PLANE:?}: the table must be sorted and hold no duplicate"
        );
    }

    /// The Studio and notebook operations are on this plane and the cluster plane's are
    /// not, so the backend's wording reaches exactly the surface it fronts.
    #[test]
    fn the_plane_holds_the_studio_and_notebook_operations_only() {
        let cases = [
            ("ListNotebookExecutions", true),
            ("ListStudioSessionMappings", true),
            ("GetStudioSessionMapping", true),
            ("CreateStudio", true),
            ("StopNotebookExecution", true),
            ("ListClusters", false),
            ("ListSteps", false),
            ("RunJobFlow", false),
            ("CreateSecurityConfiguration", false),
        ];
        for (operation, expected) in cases {
            assert_eq!(
                on_editors_plane(operation),
                expected,
                "case {operation:?}: expected {expected}, got {}",
                on_editors_plane(operation)
            );
        }
    }

    /// Each member states its own sentence: the notebook-execution status echoes the
    /// rejected value, and every other member names itself as the model spells it.
    #[test]
    fn each_enum_member_states_its_own_sentence() {
        let cases = [
            (
                "status",
                Some("NOT_A_REAL_STATUS"),
                "Invalid Execution Status: NOT_A_REAL_STATUS",
            ),
            ("status", Some("running"), "Invalid Execution Status: running"),
            ("identityType", Some("ADMIN"), "IdentityType is not valid."),
            ("identityType", Some("SUPERUSER"), "IdentityType is not valid."),
            ("authMode", Some("LDAP"), "AuthMode is not valid."),
            (
                "idcUserAssignment",
                Some("MANDATORY"),
                "IdcUserAssignment is not valid.",
            ),
            (
                "outputNotebookFormat",
                Some("PDF"),
                "OutputNotebookFormat is not valid.",
            ),
        ];
        for (member, value, expected) in cases {
            let sentence = enum_value_sentence(member, value);
            assert_eq!(
                sentence, expected,
                "case {member:?} {value:?}: expected {expected:?}, got {sentence:?}"
            );
        }
    }
}
