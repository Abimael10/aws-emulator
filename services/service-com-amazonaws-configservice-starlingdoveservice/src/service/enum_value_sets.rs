// SPDX-License-Identifier: CC0-1.0

//! The order this service LISTS an enumerated type's admissible values in when it refuses
//! a value outside that type.
//!
//! A constraint clause about an enumerated member states which values the type admits
//! (`Member must satisfy enum value set: [<values>]`). WHICH values those are is the
//! model's — the enum the member targets — but the order they are listed in is the front
//! door's: it holds the admissible values in a hash table keyed on each value's own text
//! and enumerates that TABLE rather than the model's declaration, so the listing is neither
//! the order the model declares nor a sort of it.
//!
//! `service_common::validation_message`'s one derivation of that order
//! ([`in_hashed_enum_order`]) is what states it here, so only the order of an enum's values
//! moves: the clause, the member path, the value echoed and the requirement's wording keep
//! coming from the one place that writes them
//! (`aws_smithy_schema_server::constraints::Violation::message`).
//!
//! The order is COMPUTED from the declared values themselves rather than carried as a table
//! of them, so it belongs to the enumerated TYPE and not to a member, an operation or a
//! depth: one type is listed alike wherever a value outside it is submitted, and a type the
//! model gains needs no row anywhere.

use service_common::validation_message::in_hashed_enum_order;
use smithy_server_runtime::Violation;

/// `violations` with every enumerated type's value set listed in the order this service
/// states it; every other violation is passed through unchanged.
pub(super) fn in_stated_order(violations: &[Violation]) -> Vec<Violation> {
    violations.iter().map(in_hashed_enum_order).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_server_runtime::ViolationKind;

    /// One enum violation at `path` over the value set `admitted`, as the validating walk
    /// reports one.
    fn enum_violation(path: &str, value: &str, admitted: &[&str]) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::EnumValue(
                admitted.iter().map(|value| (*value).to_string()).collect(),
            ),
            value: Some(value.to_string()),
            sensitive: false,
        }
    }

    /// The values one violation lists, which is what its clause enumerates.
    fn listed(violation: &Violation) -> Vec<String> {
        match &violation.kind {
            ViolationKind::EnumValue(values) => values.clone(),
            other => panic!("expected an enum violation, got {other:?}"),
        }
    }

    /// The snapshot-frequency type is listed in the order this service states it, whichever
    /// order the value set reached the violation in — and that set is the MODEL's own, read
    /// off the schema of the member that carries it rather than copied here.
    #[test]
    fn the_snapshot_frequency_type_is_listed_in_the_order_this_service_states() {
        // The order this service states, which is recorded behaviour: the model declares
        // the same five values `One_Hour, Three_Hours, Six_Hours, Twelve_Hours,
        // TwentyFour_Hours` (`smithy_query(query="shape",
        // shape="MaximumExecutionFrequency")`).
        let stated = [
            "TwentyFour_Hours",
            "Twelve_Hours",
            "Three_Hours",
            "Six_Hours",
            "One_Hour",
        ];
        let declared = super::super::enum_values(
            &crate::ops::types::ConfigSnapshotDeliveryProperties::SCHEMA,
            "deliveryFrequency",
        )
        .expect("the model declares deliveryFrequency and its value set");
        assert_eq!(
            declared.len(),
            stated.len(),
            "case the model's own set: expected {} values, got {declared:?}",
            stated.len()
        );

        // The rows differ along the order the set ARRIVES in — the model's declaration
        // order, a sort of it, the reverse of it, and the stated order itself — because the
        // order belongs to the type and not to how a schema presented it.
        let mut sorted: Vec<&str> = declared.to_vec();
        sorted.sort_unstable();
        let mut reversed: Vec<&str> = declared.to_vec();
        reversed.reverse();
        for (case, presented) in [
            ("the model's declaration order", declared.to_vec()),
            ("sorted", sorted),
            ("reversed", reversed),
            ("the stated order itself", stated.to_vec()),
        ] {
            let moved = in_stated_order(&[enum_violation(
                "deliveryChannel.configSnapshotDeliveryProperties.deliveryFrequency",
                "One_Minute",
                &presented,
            )]);
            assert_eq!(
                listed(&moved[0]),
                stated,
                "case {case:?}: expected the set listed as this service states it, got {:?}",
                listed(&moved[0])
            );
            assert_eq!(
                (moved[0].path.as_str(), moved[0].value.as_deref()),
                (
                    "deliveryChannel.configSnapshotDeliveryProperties.deliveryFrequency",
                    Some("One_Minute")
                ),
                "case {case:?}: the position and the echoed value must be kept"
            );
        }
    }

    /// One type, ONE listing, whatever member of whatever operation carried the value and
    /// whatever the value was: the listing is a property of the type.
    #[test]
    fn one_type_is_listed_alike_wherever_a_value_outside_it_is_submitted() {
        // Every member of this model that targets `MaximumExecutionFrequency`
        // (`smithy_query(query="shape", shape="ConfigRule"|"ConfigSnapshotDeliveryProperties"
        // |"OrganizationManagedRuleMetadata"|"OrganizationCustomRuleMetadata")`), at the
        // path the walk builds for it, each with a different submitted value.
        let declared = super::super::enum_values(
            &crate::ops::types::ConfigSnapshotDeliveryProperties::SCHEMA,
            "deliveryFrequency",
        )
        .expect("the model declares deliveryFrequency and its value set");
        let positions = [
            (
                "the delivery channel's snapshot frequency",
                "deliveryChannel.configSnapshotDeliveryProperties.deliveryFrequency",
                "One_Minute",
            ),
            (
                "a rule's maximum execution frequency",
                "configRule.maximumExecutionFrequency",
                "one_hour",
            ),
            (
                "an organization managed rule's",
                "organizationManagedRuleMetadata.maximumExecutionFrequency",
                "",
            ),
            (
                "an organization custom rule's",
                "organizationCustomRuleMetadata.maximumExecutionFrequency",
                "Forty_Eight_Hours",
            ),
        ];

        let mut one_listing: Option<Vec<String>> = None;
        for (case, path, value) in positions {
            let moved = in_stated_order(&[enum_violation(path, value, declared)]);
            let listing = listed(&moved[0]);
            assert!(
                listing.len() == declared.len()
                    && declared.iter().all(|admitted| listing
                        .iter()
                        .any(|listed| listed == admitted)),
                "case {case:?}: expected the model's own values, got {listing:?}"
            );
            match &one_listing {
                Some(first) => assert_eq!(
                    &listing, first,
                    "case {case:?}: expected the one listing {first:?}, got {listing:?}"
                ),
                None => one_listing = Some(listing),
            }
        }
    }

    /// A violation of another constraint class is passed through: only an enumerated type's
    /// listing moves.
    #[test]
    fn a_violation_of_another_class_is_passed_through() {
        for (case, violation) in [
            (
                "a length bound",
                Violation {
                    path: "deliveryChannel.name".to_string(),
                    kind: ViolationKind::MaxLength(256),
                    value: Some("c".repeat(257)),
                    sensitive: false,
                },
            ),
            (
                "an absent required member",
                Violation {
                    path: "deliveryChannel".to_string(),
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    sensitive: false,
                },
            ),
            (
                "a pattern",
                Violation {
                    path: "requesterAccountId".to_string(),
                    kind: ViolationKind::Pattern(r"^\d{12}$".to_string()),
                    value: Some("not-an-acct1".to_string()),
                    sensitive: false,
                },
            ),
        ] {
            let moved = in_stated_order(std::slice::from_ref(&violation));
            assert_eq!(
                moved,
                vec![violation.clone()],
                "case {case:?}: expected the violation passed through unchanged, got {moved:?}"
            );
        }
    }

    /// Every violation of ONE request comes back, in the order the walk found them: the
    /// listing moves inside a clause, never the clauses themselves.
    #[test]
    fn every_violation_of_a_request_is_carried_through_in_order() {
        let declared = super::super::enum_values(
            &crate::ops::types::ConfigSnapshotDeliveryProperties::SCHEMA,
            "deliveryFrequency",
        )
        .expect("the model declares deliveryFrequency and its value set");
        let absent = Violation {
            path: "deliveryChannel.s3BucketName".to_string(),
            kind: ViolationKind::MissingRequiredMember,
            value: None,
            sensitive: false,
        };
        let outside = enum_violation(
            "deliveryChannel.configSnapshotDeliveryProperties.deliveryFrequency",
            "One_Minute",
            declared,
        );
        let moved = in_stated_order(&[absent.clone(), outside]);
        assert_eq!(
            moved.len(),
            2,
            "case two violations: both must come back, got {moved:?}"
        );
        assert_eq!(
            moved[0], absent,
            "case two violations: the first must be unchanged, got {:?}",
            moved[0]
        );
        assert_eq!(
            listed(&moved[1]).first().map(String::as_str),
            Some("TwentyFour_Hours"),
            "case two violations: the enum's set must be the stated listing, got {:?}",
            listed(&moved[1])
        );
    }
}
