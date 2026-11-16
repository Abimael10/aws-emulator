// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the framework refused before any handler ran.
//!
//! AWS IoT answers a request its validation layer will not accept with the error shape
//! this service's front door names for one — `InvalidRequestException`, the legacy IoT
//! exception family, rather than the generic constraint shape the framework picks — and
//! with AWS's own envelope over one clause per constraint the request broke:
//! `N validation error(s) detected: Value '<value>' at '<member path>' failed to satisfy
//! constraint: <requirement>`. The envelope and the clauses are
//! [`::service_common::validation_message::aws_validation_refusal_withholding_violations`]'s
//! — the member paths and every requirement are the framework's, so this service cannot
//! drift from them — and only the error code is this service's own.
//!
//! The other thing chosen here is the VALUE half of a clause, per constraint CLASS:
//!
//! * a member the request did not carry has no value to quote, and AWS states that clause
//!   with none at all — `Value at '<member path>' failed to satisfy constraint: Member must
//!   not be null` — where the framework writes the `null` it read;
//! * a member outside its `@pattern` is stated with no value either: what the clause names
//!   is the expression the value had to match, and the value itself is not echoed back into
//!   the response;
//! * every other class keeps the value the request submitted — a value outside an
//!   enumerated set is quoted beside the set it had to be in — which is what the framework
//!   already states.
//!
//! Nothing here is keyed to an operation, to a member path or to a submitted value, so one
//! rule covers every `@required` member and every `@pattern` of every operation alike. How
//! that shape name is NAMED on the wire — qualified with the service's API doc namespace —
//! is `ValidationRefusalErrorType`'s, beside the operations it is known to be qualified
//! for.

use ::smithy_server_runtime::{Refusal, Refused, Violation, ViolationKind};

/// How this service states `refusal`, or `None` to keep the framework's own wording.
///
/// `None` covers both what this service does not reword (an authentication refusal is no
/// input-constraint failure, and its code and status are not this service's to change) and
/// a refusal with nothing to state.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    ::service_common::validation_message::aws_validation_refusal_withholding_violations(
        refusal,
        states_no_value,
    )
    .map(|stated| Refused {
        code: super::INVALID_REQUEST_EXCEPTION.to_owned(),
        message: stated.message,
    })
}

/// Whether the clause for `violation` states no value: the member was ABSENT, so there is
/// no value to quote; it fell outside its `@pattern`, whose clause names the expression
/// rather than what was submitted; or it is one this service never discloses.
fn states_no_value(violation: &Violation) -> bool {
    matches!(
        violation.kind,
        ViolationKind::MissingRequiredMember | ViolationKind::Pattern(_)
    ) || violation.sensitive
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One violation of `kind` at `path`, holding `value`.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_owned(),
            kind,
            value: value.map(str::to_owned),
            sensitive: false,
        }
    }

    /// RULE: a member the request did not carry and a member outside its `@pattern` are
    /// each stated with NO value, every other constraint class states the value the
    /// request submitted, and a `@pattern` is quoted without the anchors a whole-value
    /// pattern wraps it in.
    #[test]
    fn an_absent_member_states_no_value_and_every_other_class_states_one() {
        // The rows span the classes the framework reports: an absent member, a value
        // outside a pattern, one past a length bound and one outside an enumerated set.
        let rows = [
            (
                "a member the request did not carry",
                violation("policyDocument", ViolationKind::MissingRequiredMember, None),
                "1 validation error detected: Value at 'policyDocument' failed to satisfy \
                 constraint: Member must not be null",
            ),
            (
                "a member outside its pattern",
                violation(
                    "policyName",
                    ViolationKind::Pattern("^[\\w+=,.@-]+$".to_owned()),
                    Some("not a policy name"),
                ),
                "1 validation error detected: Value at 'policyName' \
                 failed to satisfy constraint: Member must satisfy regular expression \
                 pattern: [\\w+=,.@-]+",
            ),
            (
                "a member past its length maximum",
                violation("policyName", ViolationKind::MaxLength(128), Some("pppp")),
                "1 validation error detected: Value 'pppp' at 'policyName' failed to satisfy \
                 constraint: Member must have length less than or equal to 128",
            ),
            (
                "a member outside its value set",
                violation(
                    "status",
                    ViolationKind::EnumValue(vec!["ACTIVE".to_owned()]),
                    Some("MAYBE"),
                ),
                "1 validation error detected: Value 'MAYBE' at 'status' failed to satisfy \
                 constraint: Member must satisfy enum value set: [ACTIVE]",
            ),
        ];

        for (case, violation, expected) in rows {
            let stated = refused(&Refusal::Invalid(vec![violation]))
                .unwrap_or_else(|| panic!("case {case:?}: expected a stated refusal"));
            assert_eq!(
                stated.code,
                super::super::INVALID_REQUEST_EXCEPTION,
                "case {case:?}: expected this service's own refusal class, got {:?}",
                stated.code
            );
            assert_eq!(
                stated.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }
    }

    /// RULE: a refusal that is no constraint violation keeps the framework's own wording.
    #[test]
    fn a_refusal_that_is_no_constraint_violation_is_left_alone() {
        for (case, refusal) in [
            ("no violations at all", Refusal::Invalid(Vec::new())),
            ("an unauthenticated caller", Refusal::Unauthenticated),
        ] {
            assert!(
                refused(&refusal).is_none(),
                "case {case:?}: expected the framework's own wording to stand"
            );
        }
    }
}
