// SPDX-License-Identifier: CC0-1.0

//! The order Amazon Redshift lists an enumerated type's value set in when it
//! refuses a value outside it.
//!
//! A constraint clause about an enum member states which values that type admits
//! (`Member must satisfy enum value set: [<values>]`). The values are the
//! model's, but the ORDER they are listed in is the service's own: it is neither
//! the order the model declares them in nor a sort of them, so it cannot be
//! derived from the model and is carried here, one row per enumerated type this
//! service publishes an order for.
//!
//! The rows are this service's data; everything done WITH them is
//! `service_common::validation_message`'s — [`StatedEnumValueSets`] is its row
//! type and [`in_stated_enum_order`] its reader — so the CLAUSE, the envelope and
//! the code stay with the places that write them (the shared rendering, and
//! `crate::service`'s one input-refusal code) and only the order of a type's
//! values moves. A row is matched on the value SET, so every member targeting one
//! type — of any operation, at any depth — states that type's set in the one
//! order, and a type this service pins no order of is stated as the model
//! declares it.

use crate::ops::types::SOURCE_TYPE_VALUES;
use service_common::validation_message::{in_stated_enum_order, StatedEnumValueSets};
use smithy_server_runtime::Refusal;

/// `com.amazonaws.redshift#SourceType`'s five values in the order this service
/// STATES them, which is recorded behaviour: it is neither the model's
/// declaration order (`SOURCE_TYPE_VALUES`, from
/// `smithy_query(query="shape", shape="SourceType")`) nor a sort of it.
const SOURCE_TYPE_STATED: &[&str] = &[
    "cluster-security-group",
    "cluster-parameter-group",
    "scheduled-action",
    "cluster-snapshot",
    "cluster",
];

/// Every enumerated type this service publishes an order for.
///
/// One row per type whose stated order is known to differ from the order it
/// reaches a clause in; a value set absent from this table is stated as the model
/// declares it, because this service has shown no other order for it.
const STATED_ENUM_VALUE_SETS: &StatedEnumValueSets<'static> =
    &[(SOURCE_TYPE_VALUES, SOURCE_TYPE_STATED)];

/// `refusal` with every enum value set stated in the order this service lists it
/// ([`STATED_ENUM_VALUE_SETS`]); every other violation, every set no order is
/// published for, and every refusal that is not about a request's values at all
/// are carried through unchanged.
///
/// Only the ORDER inside a clause moves: the member paths, the echoed values, the
/// requirements and the number of clauses all come back as the framework's walk
/// reported them, so the rendering that runs afterwards cannot tell a restated
/// refusal from one of its own.
pub(crate) fn in_stated_order(refusal: &Refusal) -> Refusal {
    let Refusal::Invalid(violations) = refusal else {
        return refusal.clone();
    };
    Refusal::Invalid(
        violations
            .iter()
            .map(|violation| in_stated_enum_order(violation, STATED_ENUM_VALUE_SETS))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_server_runtime::{Violation, ViolationKind};

    /// One enum violation at `path` over `allowed`, as the validating walk
    /// reports one.
    fn enum_violation(path: &str, value: &str, allowed: &[&str]) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::EnumValue(allowed.iter().map(|v| (*v).to_string()).collect()),
            value: Some(value.to_string()),
            sensitive: false,
        }
    }

    /// The violations one restated refusal carries.
    fn restated(refusal: &Refusal) -> Vec<Violation> {
        match in_stated_order(refusal) {
            Refusal::Invalid(violations) => violations,
            other => panic!("expected an input refusal, got {other:?}"),
        }
    }

    /// RULE: a published value set is stated in this service's order whatever
    /// order it reached the clause in, and nothing else about the clause moves.
    #[test]
    fn a_published_set_is_stated_in_its_order_whatever_order_it_arrives_in() {
        // The model's declaration order, a reversal of it, and the published
        // order itself.
        let reversed: Vec<&str> = SOURCE_TYPE_VALUES.iter().rev().copied().collect();
        let presented: [Vec<&str>; 3] = [
            SOURCE_TYPE_VALUES.to_vec(),
            reversed,
            SOURCE_TYPE_STATED.to_vec(),
        ];
        for set in presented {
            let moved = restated(&Refusal::Invalid(vec![enum_violation(
                "sourceType",
                "nope",
                &set,
            )]));
            assert_eq!(
                moved[0].kind,
                ViolationKind::EnumValue(
                    SOURCE_TYPE_STATED.iter().map(|v| (*v).to_string()).collect()
                ),
                "case presented {set:?}: unexpected value set, got {:?}",
                moved[0].kind
            );
            assert_eq!(
                (moved[0].path.as_str(), moved[0].value.as_deref()),
                ("sourceType", Some("nope")),
                "case presented {set:?}: the position and the echoed value must be kept"
            );
        }
    }

    /// RULE: everything the table pins no order of is carried through exactly as
    /// the walk reported it, so the restating cannot change a refusal it is not
    /// about.
    #[test]
    fn everything_with_no_published_set_is_carried_through_unchanged() {
        let rows: [(&str, Vec<Violation>); 4] = [
            ("no violation at all", Vec::new()),
            (
                "a set this service pins no order of",
                vec![enum_violation("sortOrder", "sideways", &["ASC", "DESC"])],
            ),
            (
                "a violation of another class",
                vec![Violation {
                    path: "maxRecords".to_string(),
                    kind: ViolationKind::MaxValue("100".to_string()),
                    value: Some("1000".to_string()),
                    sensitive: false,
                }],
            ),
            (
                "an absent required member",
                vec![Violation {
                    path: "clusterIdentifier".to_string(),
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    sensitive: false,
                }],
            ),
        ];
        for (case, violations) in rows {
            let moved = restated(&Refusal::Invalid(violations.clone()));
            assert_eq!(
                moved, violations,
                "case {case:?}: expected the walk's own violations, got {moved:?}"
            );
        }

        // A refusal that is not about a request's values at all has no clause to
        // restate, and comes back as it arrived.
        let other = in_stated_order(&Refusal::Unauthenticated);
        assert_eq!(
            other,
            Refusal::Unauthenticated,
            "case \"an authentication refusal\": expected it unchanged, got {other:?}"
        );
    }

    /// RULE: a refusal carrying both a published set and another clause states
    /// the set in this service's order and leaves the other clause alone, in the
    /// order the walk collected them.
    #[test]
    fn a_published_set_moves_without_disturbing_the_other_clauses() {
        let other = Violation {
            path: "maxRecords".to_string(),
            kind: ViolationKind::MinValue("20".to_string()),
            value: Some("1".to_string()),
            sensitive: false,
        };
        let moved = restated(&Refusal::Invalid(vec![
            enum_violation("sourceType", "redshift-cluster", SOURCE_TYPE_VALUES),
            other.clone(),
        ]));
        assert_eq!(
            moved,
            vec![
                enum_violation("sourceType", "redshift-cluster", SOURCE_TYPE_STATED),
                other,
            ],
            "case \"two clauses\": only the published set may move, got {moved:?}"
        );
    }
}
