// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the framework refused before any handler ran.
//!
//! The centralized walk enforces the modelled input constraints and hands [`Refusal`] to
//! `service.rs` STRUCTURALLY — one [`Violation`] per offending member, carrying the dotted
//! path it sits at, what it failed and what it held — because no model records the sentence
//! a service says. This module is the ONE choice this service makes about that sentence, and
//! it makes it for a constraint CLASS rather than for an operation or a member:
//!
//! * a member ABSENT from the request is stated with NO value token at all —
//!   `Value at '<member path>' failed to satisfy constraint: Member must not be null` —
//!   because there was no value to state: the request's shape is what the clause reports, and
//!   a `null` written there would read as a value the caller supplied. Every clause about a
//!   value the request DID carry keeps that value, which is the half a caller fixes the
//!   request from.
//!
//! The envelope, the clause, the member paths, the `@pattern` rendering and every
//! constraint class's requirement belong to [`service_common::validation_message`] — the
//! shared home for this wording — and to the framework behind it. None of them is spelled
//! here, so this service cannot word a constraint differently from the services that state
//! the same sentence.

use service_common::validation_message::aws_validation_refusal_withholding_violations;
use smithy_server_runtime::{Refusal, Refused, Violation, ViolationKind};

/// How this service states `refusal`, or `None` to keep the framework's wording.
///
/// `None` for anything that is not an input-constraint refusal: an authentication refusal is
/// a different failure with its own code and status, and nothing about it is this service's
/// to word. The code travels with the message and stays the modelled `ValidationException`
/// (HTTP 400) a client branches on.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    aws_validation_refusal_withholding_violations(refusal, states_no_value)
}

/// Whether the clause for `violation` states no value: a member ABSENT from the request,
/// which had no value to state, and nothing else.
fn states_no_value(violation: &Violation) -> bool {
    matches!(violation.kind, ViolationKind::MissingRequiredMember)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One violation of `kind` at `path`, for a value the request carried.
    fn supplied(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_owned(),
            kind,
            value: Some("submitted".to_owned()),
            sensitive: false,
        }
    }

    /// An ABSENT required member is stated with no value token: the clause names the member
    /// and the requirement, and nothing between `Value` and `at`. The rows span the member
    /// paths this model's required members sit at — a top-level member, a member of a nested
    /// structure, and a member inside a list element — so the rule is the class's and not one
    /// member's.
    #[test]
    fn an_absent_required_member_is_stated_with_no_value() {
        for path in [
            "name",
            "roleArn",
            "trainingData",
            "trainingData.1.member.inputConfig",
            "trainingData.1.member.inputConfig.dataSource.glueDataSource.tableName",
        ] {
            let absent = Violation {
                path: path.to_owned(),
                kind: ViolationKind::MissingRequiredMember,
                value: None,
                sensitive: false,
            };
            let stated = refused(&Refusal::Invalid(vec![absent]))
                .expect("an absent required member is a constraint refusal");
            let expected = format!(
                "1 validation error detected: Value at '{path}' failed to satisfy constraint: \
                 Member must not be null"
            );
            assert_eq!(
                stated.message, expected,
                "case {path:?}: expected {expected:?}, got {:?}",
                stated.message
            );
            assert_eq!(
                stated.code, "ValidationException",
                "case {path:?}: the code stays the modelled one, got {:?}",
                stated.code
            );
        }
    }

    /// Every other constraint class states the value the request carried: the token this
    /// service drops belongs to the ABSENCE and to nothing else.
    #[test]
    fn a_supplied_value_is_still_stated() {
        for kind in [
            ViolationKind::MinLength(1),
            ViolationKind::MaxLength(63),
            ViolationKind::Pattern("^[a-z]+$".to_owned()),
            ViolationKind::MinValue("1".to_owned()),
            ViolationKind::MaxValue("100".to_owned()),
        ] {
            let stated = refused(&Refusal::Invalid(vec![supplied("name", kind.clone())]))
                .expect("an input-constraint refusal is this service's to state");
            assert!(
                stated.message.contains("Value 'submitted' at 'name'"),
                "case {kind:?}: expected the submitted value in the clause, got {:?}",
                stated.message
            );
        }
    }

    /// An authentication refusal keeps the framework's wording: it is a different failure,
    /// with its own code and status.
    #[test]
    fn only_an_input_constraint_refusal_is_reworded() {
        assert_eq!(
            refused(&Refusal::Unauthenticated),
            None,
            "an authentication refusal is not this service's to word"
        );
    }
}
