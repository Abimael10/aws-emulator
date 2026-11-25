// SPDX-License-Identifier: CC0-1.0

//! What a constraint refusal of a value outside an enumerated type STATES: which values
//! that type admits, in which order, and whether the refused value is stated with them.
//!
//! The first of those is this service's rather than the framework's, and it belongs to the
//! enumerated TYPE rather than to a member or an operation: the values are
//! listed in the order the front door's own TABLE of the admissible values is walked —
//! the table's buckets in ascending order, each bucket being the low bits of the value's
//! own string hash ([`service_common::validation_message::in_hashed_enum_order`]) — and
//! not in the order the model declares the type's members in. That order is computed from
//! the values themselves, so it belongs to the enumerated TYPE: every member of every
//! operation targeting one enum is listed alike, at whatever depth and in whatever binding
//! the value arrived, and a type the model gains needs nothing added here.
//!
//! The VALUE half of the clause belongs to the enumerated TYPE as well. A refusal of a
//! value outside an enumerated type echoes the value the request carried — `Value
//! '<supplied value>' at '<member path>' failed to satisfy constraint: Member must satisfy
//! enum value set: [ … ]`, the same value half a `@length`, a `@range` and a `@pattern`
//! clause of the message state — except for the types whose recorded refusals state no
//! value at all ([`TYPES_STATING_NO_VALUE`]), whose clause names the member and the
//! admissible set alone. Which it is, is decided from the value SET a clause lists, so
//! every member targeting one type is stated alike, at whatever depth and in whatever
//! binding the value arrived; the decision is applied where the clauses are worded
//! (`super::constraint_envelope`), which is the one layer that writes either half.
//!
//! Nothing about the SENTENCE is this module's: the counted envelope, the clause, the
//! member path and the requirement each constraint class states live in the shared home for
//! boundary-refusal wording ([`service_common::validation_message`]) and in the framework
//! behind it, so this service cannot word a constraint differently from the services that
//! state the same sentence.

use ::service_common::validation_message::in_hashed_enum_order;
use ::smithy_server_runtime::{Violation, ViolationKind};

/// Every enumerated type of this model whose refusals state NO value at all, each as the
/// WHOLE set of values the model declares for that type — the invitation status a
/// connection invitation is filtered by (`smithy_query(query="shape",
/// shape="InvitationStatus")`, carried here as the model-derived constant rather than
/// written out, so a value the model gains reaches this rule).
///
/// A clause is matched to an entry by the value SET it lists rather than by the member it
/// is stated at, because a violation carries the set and not the shape that declared it:
/// every member targeting one of these types is stated alike, in any operation, at any
/// depth and in any binding. A type absent from here echoes the refused value, which is
/// the form every other constraint class of the same message states.
const TYPES_STATING_NO_VALUE: &[&[&str]] = &[crate::ops::types::INVITATION_STATUS_VALUES];

/// One violation as this service states it: an enumerated type's admissible values listed
/// in the order the table of them is walked, and every other violation unchanged.
pub(super) fn as_stated(violation: &Violation) -> Violation {
    in_hashed_enum_order(violation)
}

/// Whether the clause stating `violation` withholds the value the request carried: a value
/// outside one of the enumerated types whose refusals state none
/// ([`TYPES_STATING_NO_VALUE`]). Every other violation — a value outside any other
/// enumerated type, and every class that is not an enumerated one — keeps the value half
/// the framework's own walk wrote for it.
pub(super) fn withholds_value(violation: &Violation) -> bool {
    let ViolationKind::EnumValue(listed) = &violation.kind else {
        return false;
    };
    TYPES_STATING_NO_VALUE
        .iter()
        .any(|stated| is_the_same_set(stated, listed))
}

/// Whether `listed` holds exactly the values `declared` does, in any order.
fn is_the_same_set(declared: &[&str], listed: &[String]) -> bool {
    declared.len() == listed.len()
        && declared
            .iter()
            .all(|value| listed.iter().any(|held| held == value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::smithy_server_runtime::ViolationKind;

    /// The violation the framework hands over for `value`, outside the enumerated type
    /// whose admissible values are `declared`.
    fn enum_violation(declared: &[&str]) -> Violation {
        Violation {
            path: "status".to_string(),
            kind: ViolationKind::EnumValue(
                declared.iter().map(|value| (*value).to_string()).collect(),
            ),
            value: Some("NOT_A_MODELLED_VALUE".to_string()),
            sensitive: false,
        }
    }

    /// The values a clause lists, read back off the restated violation.
    fn listed(violation: &Violation) -> Vec<String> {
        match &violation.kind {
            ViolationKind::EnumValue(values) => values.clone(),
            other => panic!("expected an enum value set, got {other:?}"),
        }
    }

    /// A set is RE-ORDERED, never changed: every value the model declares is listed
    /// exactly once, whatever the set's size. The widest type this model declares
    /// (`IndustrySegment`, 37 values) has more values than the table has buckets, so
    /// several of its buckets hold a chain.
    #[test]
    fn every_declared_value_is_listed_exactly_once() {
        for declared in [
            crate::ops::types::INDUSTRY_SEGMENT_VALUES,
            crate::ops::types::PRIMARY_SOLUTION_TYPE_VALUES,
            crate::ops::types::INVITATION_STATUS_VALUES,
            crate::ops::types::CONNECTION_TYPE_VALUES,
            crate::ops::types::PROFILE_VISIBILITY_VALUES,
        ] {
            let stated = listed(&as_stated(&enum_violation(declared)));
            let mut sorted_stated = stated.clone();
            sorted_stated.sort_unstable();
            let mut sorted_declared: Vec<String> =
                declared.iter().map(|value| (*value).to_string()).collect();
            sorted_declared.sort_unstable();
            assert_eq!(
                sorted_stated, sorted_declared,
                "case {declared:?}: the listed set is the declared one re-ordered, got {stated:?}"
            );
        }
    }

    /// The two enumerated types this service's refusals are known to list a set for, each
    /// held to the order those refusals list it in — neither the declaration order the rows
    /// carry nor the sorted one.
    #[test]
    fn a_known_set_is_listed_in_the_order_the_refusals_list_it() {
        // (type, declared order, the order a refusal lists it in)
        let rows: &[(&str, &[&str], &[&str])] = &[
            (
                // `smithy_query(query="shape",shape="PrimarySolutionType")`.
                "PrimarySolutionType",
                &[
                    "SOFTWARE_PRODUCTS",
                    "CONSULTING_SERVICES",
                    "PROFESSIONAL_SERVICES",
                    "MANAGED_SERVICES",
                    "HARDWARE_PRODUCTS",
                    "COMMUNICATION_SERVICES",
                    "VALUE_ADDED_RESALE_AWS_SERVICES",
                    "TRAINING_SERVICES",
                ],
                &[
                    "TRAINING_SERVICES",
                    "COMMUNICATION_SERVICES",
                    "CONSULTING_SERVICES",
                    "PROFESSIONAL_SERVICES",
                    "VALUE_ADDED_RESALE_AWS_SERVICES",
                    "HARDWARE_PRODUCTS",
                    "SOFTWARE_PRODUCTS",
                    "MANAGED_SERVICES",
                ],
            ),
            (
                // `smithy_query(query="shape",shape="InvitationStatus")`.
                "InvitationStatus",
                &["PENDING", "ACCEPTED", "REJECTED", "CANCELED", "EXPIRED"],
                &["EXPIRED", "ACCEPTED", "PENDING", "CANCELED", "REJECTED"],
            ),
        ];

        for (case, declared, stated) in rows {
            let got = listed(&as_stated(&enum_violation(declared)));
            assert_eq!(
                got, *stated,
                "case {case:?}: the set is listed in the order the refusals list it in, got \
                 {got:?}"
            );
        }
    }

    /// Re-ordering a set restates the set and NOTHING else: the member path, the rejected
    /// value and whether that value is withheld reach the clause exactly as the
    /// framework's walk reported them, whatever the constraint class.
    #[test]
    fn only_the_listed_set_is_restated() {
        let refused = enum_violation(crate::ops::types::INVITATION_STATUS_VALUES);
        let rows = [
            ("a value outside an enum", refused.clone()),
            (
                "a length maximum",
                Violation {
                    kind: ViolationKind::MaxLength(64),
                    ..refused.clone()
                },
            ),
            (
                "a length minimum",
                Violation {
                    kind: ViolationKind::MinLength(1),
                    ..refused.clone()
                },
            ),
            (
                "a range maximum",
                Violation {
                    kind: ViolationKind::MaxValue("100".to_string()),
                    ..refused.clone()
                },
            ),
            (
                "a pattern",
                Violation {
                    kind: ViolationKind::Pattern("^[a-zA-Z0-9-]+$".to_string()),
                    ..refused.clone()
                },
            ),
            (
                "a value the walk withholds",
                Violation {
                    sensitive: true,
                    ..refused.clone()
                },
            ),
            (
                "an absent required member",
                Violation {
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    ..refused.clone()
                },
            ),
        ];

        for (case, violation) in rows {
            let stated = as_stated(&violation);
            assert_eq!(
                (&stated.path, &stated.value, stated.sensitive),
                (&violation.path, &violation.value, violation.sensitive),
                "case {case:?}: expected the path and value half of {violation:?}, got {stated:?}"
            );
        }
    }

    /// Whether the value is withheld follows the enumerated TYPE: the types whose
    /// refusals state none withhold it and every other type of this model echoes it,
    /// whichever member, operation or binding the value arrived in — the decision reads
    /// the value set alone, and nothing of the path or the value.
    #[test]
    fn only_the_types_stating_no_value_withhold_it() {
        // (case, the type's modelled value set)
        let rows: &[(&str, &[&str])] = &[
            ("InvitationStatus", crate::ops::types::INVITATION_STATUS_VALUES),
            (
                "PrimarySolutionType",
                crate::ops::types::PRIMARY_SOLUTION_TYPE_VALUES,
            ),
            ("ConnectionType", crate::ops::types::CONNECTION_TYPE_VALUES),
            ("ParticipantType", crate::ops::types::PARTICIPANT_TYPE_VALUES),
            (
                "ProfileVisibility",
                crate::ops::types::PROFILE_VISIBILITY_VALUES,
            ),
            ("AccessType", crate::ops::types::ACCESS_TYPE_VALUES),
            ("IndustrySegment", crate::ops::types::INDUSTRY_SEGMENT_VALUES),
            ("VerificationType", crate::ops::types::VERIFICATION_TYPE_VALUES),
        ];

        for (case, declared) in rows {
            let withholds = TYPES_STATING_NO_VALUE
                .iter()
                .any(|stated| stated == declared);
            for path in ["status", "taskDetails.primarySolutionType"] {
                let violation = Violation {
                    path: path.to_string(),
                    ..enum_violation(declared)
                };
                assert_eq!(
                    withholds_value(&violation),
                    withholds,
                    "case {case:?} at {path:?}: expected withholding to be {withholds}, got \
                     {violation:?}"
                );
                // The set reaches the clause re-ordered, which must not change the answer.
                assert_eq!(
                    withholds_value(&as_stated(&violation)),
                    withholds,
                    "case {case:?} at {path:?}: the re-ordered set is the same set, got \
                     {violation:?}"
                );
            }
        }
    }

    /// Every OTHER constraint class keeps the value half the walk wrote for it, whatever
    /// member it broke — including a member of a type whose enum refusals state no value.
    #[test]
    fn no_other_class_withholds_its_value() {
        let refused = enum_violation(crate::ops::types::INVITATION_STATUS_VALUES);
        let rows = [
            ("a length maximum", ViolationKind::MaxLength(64)),
            ("a length minimum", ViolationKind::MinLength(1)),
            ("a range maximum", ViolationKind::MaxValue("100".to_string())),
            ("a range minimum", ViolationKind::MinValue("1".to_string())),
            (
                "a pattern",
                ViolationKind::Pattern("^[a-zA-Z0-9-]+$".to_string()),
            ),
            ("an absent required member", ViolationKind::MissingRequiredMember),
        ];

        for (case, kind) in rows {
            let violation = Violation {
                kind,
                ..refused.clone()
            };
            assert!(
                !withholds_value(&violation),
                "case {case:?}: expected the walk's own value half, got a withheld one for \
                 {violation:?}"
            );
        }
    }
}
