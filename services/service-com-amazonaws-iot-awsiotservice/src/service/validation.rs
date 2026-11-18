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
//!   is the expression the value had to match, quoted without the whole-value anchors the
//!   model wraps it in, and the value itself is not echoed back into the response. That is
//!   what this service's recorded refusals of a URI-LABEL member show; whether a `@pattern`
//!   on a BODY member is stated the same way is a question about this rendering that a
//!   report of its own carries, and the rendering lives in `service_common` either way;
//! * a member outside either end of its `@length` is stated with no value as well: the
//!   clause names the member and the bound it broke, and the text that was too long or too
//!   short — which is the whole of what the caller sent for that member — is not reflected
//!   back. A value and a length bound say the same thing twice, and this service's recorded
//!   refusals of a member that broke its bound AND its expression together state neither
//!   clause with a value;
//! * every other class keeps the value the request submitted — a value outside an
//!   enumerated set is quoted beside the set it had to be in — which is what the framework
//!   already states.
//!
//! The last thing chosen here is the ORDER an enumerated type's value set is listed in: the
//! front door in front of this service holds a type's admissible values in a HASH TABLE
//! keyed on each value's own text and enumerates that table, so the set is stated in
//! ascending bucket order rather than in the order the model declares the type
//! ([`::service_common::validation_message::in_hashed_enum_order`], whose own docs carry the
//! derivation). It is computed from the declared values themselves, so it belongs to the
//! enumerated TYPE: every enum of this model is listed that way and a type the model gains
//! needs nothing here.
//!
//! Nothing here is keyed to an operation, to a member path or to a submitted value, so one
//! rule covers every `@required` member, every `@pattern` and every enum of every operation
//! alike.
//!
//! The error-shape NAME this chooses is the bare one, because naming it on the wire is not
//! this seam's: the shape name a chosen refusal carries is reduced by the framework to the
//! characters a shape id holds, so the namespace AWS IoT qualifies it with cannot travel
//! through here. `ShapeNamespacedErrorType`, in front of every operation, is what writes
//! that name — for this shape wherever it comes from, so a refusal worded here and one a
//! handler answers name it identically.

use ::smithy_server_runtime::{Refusal, Refused, Violation, ViolationKind};

/// How this service states `refusal`, or `None` to keep the framework's own wording.
///
/// `None` covers both what this service does not reword (an authentication refusal is no
/// input-constraint failure, and its code and status are not this service's to change) and
/// a refusal with nothing to state.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    ::service_common::validation_message::aws_validation_refusal_withholding_violations(
        &in_stated_value_set_order(refusal),
        states_no_value,
    )
    .map(|stated| Refused {
        code: super::INVALID_REQUEST_EXCEPTION.to_owned(),
        message: stated.message,
    })
}

/// `refusal` with every enumerated type's value set in the order this service's front door
/// states it, and every other violation untouched.
fn in_stated_value_set_order(refusal: &Refusal) -> Refusal {
    match refusal {
        Refusal::Invalid(violations) => Refusal::Invalid(
            violations
                .iter()
                .map(::service_common::validation_message::in_hashed_enum_order)
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Whether the clause for `violation` states no value: the member was ABSENT, so there is
/// no value to quote; it fell outside its `@pattern`, whose clause names the expression
/// rather than what was submitted; it fell outside its `@length`, whose clause names the
/// bound rather than the text that broke it; or it is one this service never discloses.
fn states_no_value(violation: &Violation) -> bool {
    matches!(
        violation.kind,
        ViolationKind::MissingRequiredMember
            | ViolationKind::Pattern(_)
            | ViolationKind::MinLength(_)
            | ViolationKind::MaxLength(_)
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

    /// RULE: a member the request did not carry, a member outside its `@pattern` and a
    /// member outside its `@length` are each stated with NO value, every other constraint
    /// class states the value the request submitted, and a `@pattern` is quoted without
    /// the anchors a whole-value pattern wraps it in.
    #[test]
    fn an_absent_member_states_no_value_and_every_other_class_states_one() {
        // The rows span the classes the framework reports: an absent member, a value
        // outside a pattern, one past each end of a length bound and one outside an
        // enumerated set.
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
                "1 validation error detected: Value at 'policyName' failed to satisfy \
                 constraint: Member must have length less than or equal to 128",
            ),
            (
                "a member short of its length minimum",
                violation("targetAwsAccount", ViolationKind::MinLength(12), Some("12345")),
                "1 validation error detected: Value at 'targetAwsAccount' failed to satisfy \
                 constraint: Member must have length greater than or equal to 12",
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
            (
                // `LogTargetType`, declared DEFAULT, THING_GROUP, CLIENT_ID, SOURCE_IP,
                // PRINCIPAL_ID, DEVICE_DEFENDER
                // (`smithy_query(query="shape",shape="LogTargetType")`), is stated in its
                // hash table's bucket order — 1, 11, 12, 12, 14, 15 — which is neither the
                // declaration order nor the alphabetical one, and where two values share a
                // bucket the later-declared one is stated first.
                "a member outside a value set of several members",
                violation(
                    "targetType",
                    ViolationKind::EnumValue(vec![
                        "DEFAULT".to_owned(),
                        "THING_GROUP".to_owned(),
                        "CLIENT_ID".to_owned(),
                        "SOURCE_IP".to_owned(),
                        "PRINCIPAL_ID".to_owned(),
                        "DEVICE_DEFENDER".to_owned(),
                    ]),
                    Some("NOT_A_REAL_TYPE"),
                ),
                "1 validation error detected: Value 'NOT_A_REAL_TYPE' at 'targetType' failed \
                 to satisfy constraint: Member must satisfy enum value set: [DEFAULT, \
                 SOURCE_IP, DEVICE_DEFENDER, PRINCIPAL_ID, THING_GROUP, CLIENT_ID]",
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
