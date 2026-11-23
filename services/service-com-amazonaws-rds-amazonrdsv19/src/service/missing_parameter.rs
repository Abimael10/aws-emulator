// SPDX-License-Identifier: CC0-1.0

//! How this front door refuses a request that left a parameter the operation requires out.
//!
//! An ABSENT parameter and a parameter carrying a value its member does not accept are two
//! different claims about a request, and this service words them differently: a value
//! outside a declared constraint is reported as the constraint it broke, with the value, the
//! member it sat at and the requirement (the framework's own clause, which is the text the
//! service answers that class with); a parameter that was never sent has no value to report,
//! so the service states only that one is needed and which parameter needs it.
//!
//! The sentence is entirely FIXED but for the parameter it names, which is read off the
//! refusal itself — the member position the framework's validating walk recorded, spelled as
//! that walk spells a member in a refusal (`aws_smithy_schema_server::constraints::path`:
//! the model's member name with its first character lowered, so `Engine` is named `engine`).
//! Nothing else of the request reaches it.
//!
//! It holds for every operation this door serves and for every member a request can leave
//! out, because the claim is the same one each time, so it lives here — on the one refusal
//! seam every request passes through (`crate::service`) — rather than in the handlers, which
//! never run for a request the boundary already refused.

use aws_smithy_schema_server::{Violation, ViolationKind};

/// What Amazon RDS refuses a request missing a required parameter with.
///
/// `parameter` is the parameter the request has to carry, in the spelling a refusal of this
/// service names a member with; the rest is fixed text, terminated by a period.
fn missing_parameter_message(parameter: &str) -> String {
    format!("Please specify a value for parameter '{parameter}'.")
}

/// The sentence for the parameter a request left out, or `None` when every violation this
/// refusal carries is about a value that WAS sent.
///
/// The sentence names ONE parameter, so the first member the walk found absent is the one it
/// names, and it is preferred over any value constraint the same request also broke: a value
/// can only be held to its member's domain once it is there, so the parameter that is missing
/// is what the caller has to supply first.
pub fn message(violations: &[Violation]) -> Option<String> {
    violations
        .iter()
        .find(|violation| violation.kind == ViolationKind::MissingRequiredMember)
        .map(|violation| missing_parameter_message(&violation.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A violation of `kind` at `path`, as the validating walk records one.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    #[test]
    fn the_absent_parameter_is_named_in_the_services_own_sentence() {
        let stated = message(&[violation("engine", ViolationKind::MissingRequiredMember, None)]);
        assert_eq!(
            stated.as_deref(),
            Some("Please specify a value for parameter 'engine'."),
            "an absent required member is refused by naming the parameter, got {stated:?}"
        );
    }

    #[test]
    fn a_value_constraint_is_not_a_missing_parameter() {
        let stated = message(&[violation(
            "maxRecords",
            ViolationKind::MinValue("20".to_string()),
            Some("1"),
        )]);
        assert_eq!(
            stated, None,
            "a value that WAS sent is no missing parameter, got {stated:?}"
        );
    }

    #[test]
    fn the_absent_parameter_is_named_ahead_of_a_value_that_was_sent() {
        let stated = message(&[
            violation("maxRecords", ViolationKind::MaxValue("100".to_string()), Some("500")),
            violation("engine", ViolationKind::MissingRequiredMember, None),
        ]);
        assert_eq!(
            stated.as_deref(),
            Some("Please specify a value for parameter 'engine'."),
            "the parameter the request has to supply first is the absent one, got {stated:?}"
        );
    }

    #[test]
    fn a_refusal_carrying_no_violation_states_nothing() {
        let stated = message(&[]);
        assert_eq!(stated, None, "nothing was refused, got {stated:?}");
    }
}
