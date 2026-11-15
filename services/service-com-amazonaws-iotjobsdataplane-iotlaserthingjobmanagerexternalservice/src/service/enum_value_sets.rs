// SPDX-License-Identifier: CC0-1.0

//! The ORDER this plane lists an enumerated type's admissible values in when it refuses a
//! value outside that type.
//!
//! RULE: a boundary refusal of a value outside an enumerated type states that type's value
//! set in the order the front door's own TABLE of the admissible values is walked — the
//! table's buckets in ascending order, a bucket being the low bits of the value's own string
//! hash ([`service_common::validation_message::in_hashed_enum_order`]) — and not in the
//! order the model declares the type's members in.
//!
//! The order is computed from the declared values themselves, so it belongs to the
//! enumerated TYPE: every member of every operation targeting one enum is listed alike, at
//! whatever depth and in whatever binding the value arrived, and a type this model gains
//! needs nothing added here.
//!
//! Nothing about the SENTENCE is this module's: the counted envelope, the clause, the member
//! path, the value the clause echoes and every other constraint class's requirement come
//! from the framework that reported the violation, so this plane cannot word a constraint
//! differently from the services that state the same sentence. Only the order inside an enum
//! clause moves, and a refusal carrying no enum clause is left to the framework entirely.

use ::service_common::validation_message::in_hashed_enum_order;
use ::smithy_server_runtime::{Refusal, Refused, Violation};

use super::error_type_locator::BOUNDARY_SHAPE;

/// One violation as THIS plane states it: an enumerated type's value set listed in the order
/// the table of the admissible values is walked.
///
/// Every other violation kind comes back unchanged, which is what [`stated_refusal`] reads to
/// decide whether to reword at all.
pub(crate) fn as_this_service_states_it(violation: &Violation) -> Violation {
    in_hashed_enum_order(violation)
}

/// How this plane states a refusal the framework made before any handler ran, or `None` when
/// it states that refusal exactly as the framework already does.
///
/// Only the value set inside an enum clause moves; the counted envelope is the framework's,
/// and so is the code — the boundary shape the framework itself would have named
/// ([`BOUNDARY_SHAPE`]), restated here because `override_refusal` replaces the code and the
/// message together and a message without its matching code is worse than leaving both
/// alone. WHICH shape that refusal finally reaches a client under is
/// [`super::error_type_locator`]'s: the operation being served is not known at this seam.
pub(crate) fn stated_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let stated: Vec<Violation> = violations.iter().map(as_this_service_states_it).collect();
    if stated == *violations {
        return None;
    }
    Some(Refused {
        code: BOUNDARY_SHAPE.to_string(),
        message: ::aws_smithy_schema_server::constraints::envelope(&stated)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::smithy_server_runtime::ViolationKind;

    /// A violation of `path` carrying `value`, outside the enum whose admissible values are
    /// `declared` — the shape the framework hands this module.
    fn enum_violation(path: &str, value: &str, declared: &[&str]) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::EnumValue(
                declared.iter().map(|value| (*value).to_string()).collect(),
            ),
            value: Some(value.to_string()),
            sensitive: false,
        }
    }

    /// The order is a property of the VALUES, so one type's set is listed alike whichever
    /// order it reaches the clause in — and nothing else about the violation moves.
    #[test]
    fn one_enum_is_listed_alike_whatever_order_it_is_presented_in() {
        // `JobExecutionStatus` as the model declares it (`smithy_query(query="shape",
        // shape="JobExecutionStatus")`).
        let declared = [
            "QUEUED",
            "IN_PROGRESS",
            "SUCCEEDED",
            "FAILED",
            "TIMED_OUT",
            "REJECTED",
            "REMOVED",
            "CANCELED",
        ];
        let listed: Vec<String> =
            ::service_common::validation_message::hashed_enum_order(&declared)
                .into_iter()
                .map(str::to_string)
                .collect();
        let mut reversed = declared;
        reversed.reverse();
        let mut sorted = declared;
        sorted.sort_unstable();
        for (case, presented) in [
            ("as declared", declared),
            ("reversed", reversed),
            ("sorted", sorted),
        ] {
            let restated = as_this_service_states_it(&enum_violation(
                "status",
                "BOGUS_STATUS",
                &presented,
            ));
            assert_eq!(
                restated.kind,
                ViolationKind::EnumValue(listed.clone()),
                "case {case:?}: expected the value set listed {listed:?}, got {:?}",
                restated.kind
            );
            assert_eq!(
                (restated.path.as_str(), restated.value.as_deref()),
                ("status", Some("BOGUS_STATUS")),
                "case {case:?}: re-ordering the set changes nothing else about the violation"
            );
        }
    }

    /// A refusal that states no enum value set at all is declined whole, so the framework
    /// words it — value, clause and envelope.
    #[test]
    fn a_refusal_stating_no_enum_value_set_is_declined() {
        for (case, refusal) in [
            (
                "a length bound",
                Refusal::Invalid(vec![Violation {
                    path: "jobId".to_string(),
                    kind: ViolationKind::MaxLength(64),
                    value: Some("j".repeat(65)),
                    sensitive: false,
                }]),
            ),
            (
                "an absent required member",
                Refusal::Invalid(vec![Violation {
                    path: "status".to_string(),
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    sensitive: false,
                }]),
            ),
            ("an unauthenticated caller", Refusal::Unauthenticated),
        ] {
            let stated = stated_refusal(&refusal);
            assert!(
                stated.is_none(),
                "case {case:?}: expected the framework's own wording, got {stated:?}"
            );
        }
    }
}
