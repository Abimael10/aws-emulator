// SPDX-License-Identifier: CC0-1.0

//! The value set Amazon EC2 Auto Scaling STATES when it refuses a value outside an
//! enumerated type, and the order it lists that set in.
//!
//! A constraint clause about an enumerated member names the values the member may carry
//! (`Member must satisfy enum value set: [<values>]`). Two things about that list are the
//! SERVICE's rather than the model's:
//!
//! * WHICH values it holds. The list is what the service tells a caller it accepts, and a
//!   service whose live value set has grown past the model shipped with this emulator
//!   states the live one — so a caller reading the refusal is not told that a value the
//!   service does accept is illegal.
//! * The ORDER they are listed in. The front door holds the values of a set in a HASH
//!   TABLE keyed on each value's own text and enumerates the table rather than the type's
//!   declaration, so the list is neither the model's declaration order nor a sort of it.
//!   That derivation is `service_common`'s ([`in_hashed_enum_order`]), shared with every
//!   other emulated service whose recorded refusals list a value set that way, so it is not
//!   restated here. The table is the front door's, so the order belongs to EVERY enumerated
//!   type of this model and not to the one type a set is published for: a refusal of any
//!   enumerated member states its values in that order.
//!
//! The rows are this service's data; everything done WITH them is
//! `service_common::validation_message`'s — [`in_stated_enum_order`] is its reader — so the
//! CLAUSE, the member path, the echoed value and the envelope stay with the one place that
//! writes them (`aws_smithy_schema_server::envelope`) and only the value set moves. A row
//! is matched on the value SET the model declares, so every member targeting that type — of
//! any operation, at any depth — states the one set, and a type this service publishes no
//! set of states exactly the values the model declares, in that same table order.

use service_common::validation_message::{in_hashed_enum_order, in_stated_enum_order};
use smithy_server_runtime::Violation;

/// The values the model this emulator ships declares for the enumerated type a
/// target-tracking policy names its predefined metric from
/// (`smithy_query(query="shape",shape="MetricType")`).
///
/// This half of the row is what a refusal is MATCHED on, so it is the model's set as the
/// validating walk carries it.
const METRIC_TYPE_DECLARED: &[&str] = &[
    "ASGAverageCPUUtilization",
    "ASGAverageNetworkIn",
    "ASGAverageNetworkOut",
    "ALBRequestCountPerTarget",
];

/// The predefined metric types the SERVICE publishes for that type: the four the model
/// declares, plus the high-resolution CPU metric, which the live service accepts and the
/// shipped model does not declare.
///
/// SOURCE: the service's own refusal of a value outside the set, which lists five values —
/// these — where the model declares four. The ORDER is not written out here either: the
/// front door's table decides it ([`in_hashed_enum_order`]), exactly as it does for every
/// other set, so these are the five values in the order the model would have declared them.
///
/// Stating a value the shipped model does not declare does not ADMIT it: which values a
/// member accepts is the enum the member targets, checked by the framework's own constraint
/// walk, so the high-resolution metric is still refused if a request carries it. That gap is
/// the shipped model's staleness against the live service, and closing it is a change to the
/// model rather than to how a refusal is worded; what this row fixes is the refusal that
/// told a caller the metric does not exist at all.
const METRIC_TYPE_PUBLISHED: &[&str] = &[
    "ASGAverageCPUUtilization",
    "ASGAverageNetworkIn",
    "ASGAverageNetworkOut",
    "ALBRequestCountPerTarget",
    "ASGAverageCPUUtilizationHighResolution",
];

/// `violations` with every enumerated type's value set as the service STATES it: the set
/// this service publishes beyond the model where it publishes one, listed in the order the
/// front door's table is walked. Every violation of another class is passed through
/// unchanged.
///
/// The two steps are in that order because the published set is a set: which values are
/// stated is decided first, and the order is then derived from the values that end up being
/// stated — so a published set and a model-declared one are listed by the one derivation.
pub(crate) fn in_published_order(violations: &[Violation]) -> Vec<Violation> {
    let published = [(METRIC_TYPE_DECLARED, METRIC_TYPE_PUBLISHED)];
    violations
        .iter()
        .map(|violation| in_hashed_enum_order(&in_stated_enum_order(violation, &published)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_server_runtime::ViolationKind;

    /// One enum violation at `path` over `declared`, as the validating walk reports one.
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

    /// The values a violation carries, which is what the clause lists.
    fn stated(violation: &Violation) -> Vec<String> {
        match &violation.kind {
            ViolationKind::EnumValue(values) => values.clone(),
            other => panic!("expected an enum violation, got {other:?}"),
        }
    }

    /// The published set is stated whichever order the declared set reached the clause in,
    /// and the position and the echoed value are kept.
    #[test]
    fn the_published_set_is_stated_whatever_order_it_arrives_in() {
        let expected: Vec<String> = service_common::validation_message::hashed_enum_order(
            METRIC_TYPE_PUBLISHED,
        )
        .iter()
        .map(|value| (*value).to_string())
        .collect();
        for presented in [
            // The model's declaration order, a sort of it, and its reverse.
            vec![
                "ASGAverageCPUUtilization",
                "ASGAverageNetworkIn",
                "ASGAverageNetworkOut",
                "ALBRequestCountPerTarget",
            ],
            vec![
                "ALBRequestCountPerTarget",
                "ASGAverageCPUUtilization",
                "ASGAverageNetworkIn",
                "ASGAverageNetworkOut",
            ],
            vec![
                "ALBRequestCountPerTarget",
                "ASGAverageNetworkOut",
                "ASGAverageNetworkIn",
                "ASGAverageCPUUtilization",
            ],
        ] {
            let moved = in_published_order(&[enum_violation(
                "targetTrackingConfiguration.predefinedMetricSpecification.\
                 predefinedMetricType",
                "ASGBogusMetric",
                &presented,
            )]);
            assert_eq!(
                stated(&moved[0]),
                expected,
                "case presented {presented:?}: unexpected value set"
            );
            assert_eq!(
                (moved[0].path.as_str(), moved[0].value.as_deref()),
                (
                    "targetTrackingConfiguration.predefinedMetricSpecification.\
                     predefinedMetricType",
                    Some("ASGBogusMetric")
                ),
                "case presented {presented:?}: the position and the echoed value must be kept"
            );
        }
    }

    /// A value set no row publishes holds exactly the values the model declares — and is
    /// listed in the same table order a published one is, that order being the front door's
    /// rather than any one type's.
    #[test]
    fn an_unpublished_value_set_is_stated_in_the_tables_order() {
        let declared = ["Stopped", "Running", "Hibernated"];
        let expected: Vec<String> =
            service_common::validation_message::hashed_enum_order(&declared)
                .iter()
                .map(|value| (*value).to_string())
                .collect();
        let moved = in_published_order(&[enum_violation("poolState", "Warmed", &declared)]);
        assert_eq!(
            stated(&moved[0]),
            expected,
            "case an unpublished value set: it must be stated in the table's order"
        );
        let mut members = stated(&moved[0]);
        members.sort_unstable();
        let mut admitted: Vec<String> = declared.iter().map(|v| (*v).to_string()).collect();
        admitted.sort_unstable();
        assert_eq!(
            members, admitted,
            "case an unpublished value set: it must hold exactly the declared values"
        );
    }

    /// A violation of another class is passed through, and every violation of one refusal
    /// comes back in the order it was collected.
    #[test]
    fn every_other_violation_is_carried_through_unchanged() {
        let other = Violation {
            path: "maxSize".to_string(),
            kind: ViolationKind::MaxValue("10".to_string()),
            value: Some("11".to_string()),
            sensitive: false,
        };
        let moved = in_published_order(&[
            enum_violation("predefinedMetricType", "ASGBogusMetric", METRIC_TYPE_DECLARED),
            other.clone(),
        ]);
        assert_eq!(
            moved.len(),
            2,
            "case two violations: both must come back, got {moved:?}"
        );
        assert_eq!(
            stated(&moved[0]).len(),
            METRIC_TYPE_PUBLISHED.len(),
            "case two violations: the published set must be stated, got {:?}",
            stated(&moved[0])
        );
        assert_eq!(
            moved[1], other,
            "case two violations: the other must be unchanged"
        );
    }
}
