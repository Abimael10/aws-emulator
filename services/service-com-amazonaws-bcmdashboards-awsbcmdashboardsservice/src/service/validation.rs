// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the framework refused before any handler ran.
//!
//! The centralized walk enforces the modelled input constraints and hands [`Refusal`] to
//! [`crate::service`] STRUCTURALLY — one `Violation` per offending member, carrying the
//! dotted path it sits at, what it failed and what it held — because no model records the
//! sentence a service says. ONE choice about that sentence is this service's, and it is made
//! for a constraint CLASS rather than for an operation or a member: a `@pattern` requirement
//! is quoted as the expression this service publishes, without the pair of whole-value
//! anchors the model wraps it in. The CHECK is unchanged — it still runs the model's pattern
//! as written — so only the rendering drops the pair.
//!
//! Every constraint class otherwise states the standard AWS clause IN FULL, the value half
//! included: `Value '<the value the request submitted>' at '<member path>' failed to satisfy
//! constraint: <requirement>`. That holds for a numeric value outside its member's declared
//! `@range` as much as for a string outside a `@length` or a `@pattern` — a page size on
//! either listing, a widget's width or height on either dashboard write, at either end of
//! the range and at whatever depth the member sits — because the value a request sent is
//! what tells its caller WHICH input was rejected. A member absent from the request states
//! the `null` it was.
//!
//! The envelope, its error count, the clause, the member paths and every constraint class's
//! requirement sentence belong to [`service_common::validation_message`] — the shared home
//! for this wording — and to the framework behind it. None of them is spelled here, so this
//! service cannot word a constraint differently from the services that state the same
//! sentence. `tests/all/range_refusal_wording.rs` pins the range clause over the wire, and
//! `tests/all/refused_value_echo.rs` pins the value half over the whole numeric domain a
//! declared range refuses.

use service_common::validation_message::aws_validation_refusal;
use smithy_server_runtime::{Refusal, Refused};

/// How this service states `refusal`, or `None` to keep the framework's own wording.
///
/// `None` for anything that is not an input-constraint refusal: an authentication refusal is
/// a different failure with its own status and code, and nothing about it is this service's
/// to word. The code travels with the message and stays the modelled `ValidationException`
/// (HTTP 400) a client branches on.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    aws_validation_refusal(refusal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_server_runtime::{Violation, ViolationKind};

    /// One violation of `kind` at `path`, for the value a request sent.
    fn violation(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_owned(),
            kind,
            value: Some("101".to_owned()),
            sensitive: false,
        }
    }

    /// Either end of a declared range is stated with the member, the bound AND the value the
    /// request submitted. The wire-level counterpart, over real operations and real members,
    /// is `tests/all/range_refusal_wording.rs`.
    #[test]
    fn a_range_clause_states_the_value_the_member_and_the_bound() {
        for (path, kind, expected) in [
            (
                "maxResults",
                ViolationKind::MaxValue("100".to_owned()),
                "Member must have value less than or equal to 100",
            ),
            (
                "maxResults",
                ViolationKind::MinValue("1".to_owned()),
                "Member must have value greater than or equal to 1",
            ),
            (
                "widgets.1.member.width",
                ViolationKind::MaxValue("6".to_owned()),
                "Member must have value less than or equal to 6",
            ),
            (
                "widgets.1.member.height",
                ViolationKind::MinValue("4".to_owned()),
                "Member must have value greater than or equal to 4",
            ),
        ] {
            let stated = refused(&Refusal::Invalid(vec![violation(path, kind.clone())]))
                .expect("an input-constraint refusal is this service's to state");
            let expected = format!(
                "1 validation error detected: Value '101' at '{path}' failed to satisfy \
                 constraint: {expected}"
            );
            assert_eq!(
                stated.message, expected,
                "case {path} {kind:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }
    }

    /// Every other class states its value the same way: a string what it submitted, an
    /// absent member the `null` it was.
    #[test]
    fn another_class_states_its_value() {
        for (kind, expected) in [
            (
                ViolationKind::MinLength(20),
                "Value '101' at 'arn' failed to satisfy constraint: Member must have length \
                 greater than or equal to 20",
            ),
            (
                ViolationKind::MaxLength(50),
                "Value '101' at 'arn' failed to satisfy constraint: Member must have length \
                 less than or equal to 50",
            ),
            (
                ViolationKind::Pattern("^[0-9a-f]{32}$".to_owned()),
                "Value '101' at 'arn' failed to satisfy constraint: Member must satisfy \
                 regular expression pattern: [0-9a-f]{32}",
            ),
        ] {
            let stated = refused(&Refusal::Invalid(vec![violation("arn", kind.clone())]))
                .expect("an input-constraint refusal is this service's to state");
            let expected = format!("1 validation error detected: {expected}");
            assert_eq!(
                stated.message, expected,
                "case {kind:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }

        let absent = Violation {
            path: "name".to_owned(),
            kind: ViolationKind::MissingRequiredMember,
            value: None,
            sensitive: false,
        };
        let stated = refused(&Refusal::Invalid(vec![absent]))
            .expect("an absent required member is a constraint refusal");
        assert_eq!(
            stated.message,
            "1 validation error detected: Value null at 'name' failed to satisfy constraint: \
             Member must not be null",
            "an absent member states the null it was, got {:?}",
            stated.message
        );
    }

    /// One request breaking a range AND another class states each clause under one envelope,
    /// each with the value it submitted.
    #[test]
    fn each_class_states_its_value_under_one_envelope() {
        let stated = refused(&Refusal::Invalid(vec![
            violation("maxResults", ViolationKind::MaxValue("100".to_owned())),
            violation("nextToken", ViolationKind::MinLength(1)),
        ]))
        .expect("an input-constraint refusal is this service's to state");
        assert_eq!(
            stated.message,
            "2 validation errors detected: Value '101' at 'maxResults' failed to satisfy \
             constraint: Member must have value less than or equal to 100; Value '101' at \
             'nextToken' failed to satisfy constraint: Member must have length greater than \
             or equal to 1",
            "each class states the value it submitted, got {:?}",
            stated.message
        );
    }

    /// An authentication refusal is not this service's to word, and the code a constraint
    /// refusal travels with stays the modelled one.
    #[test]
    fn only_an_input_constraint_refusal_is_reworded() {
        assert_eq!(
            refused(&Refusal::Unauthenticated),
            None,
            "an authentication refusal is not this service's to word"
        );
        let chosen = refused(&Refusal::Invalid(vec![violation(
            "maxResults",
            ViolationKind::MaxValue("100".to_owned()),
        )]))
        .expect("an input-constraint refusal is reworded");
        assert_eq!(
            chosen.code, "ValidationException",
            "the code stays the modelled one, got {:?}",
            chosen.code
        );
    }
}
