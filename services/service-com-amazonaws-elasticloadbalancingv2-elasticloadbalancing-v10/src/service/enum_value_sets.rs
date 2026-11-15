// SPDX-License-Identifier: CC0-1.0

//! The order Elastic Load Balancing lists an enum's value set in when it refuses a value
//! outside it.
//!
//! A constraint clause about an enum member states which values the enum admits
//! (`Member must satisfy enum value set: [<values>]`). The values are the model's, but the
//! ORDER they are listed in is the service's own: it is neither the order the model
//! declares them in nor a sort of them, so it cannot be derived from the model and is
//! carried here, one row per enum this service publishes an order for.
//!
//! The rows are this service's data; everything done WITH them is
//! `service_common::validation_message`'s — [`StatedEnumValueSets`] is its row type and
//! [`in_stated_enum_order`] its reader, so the CLAUSE and the envelope stay with the one
//! place that writes them (`aws_smithy_schema_server::envelope`) and only the order of an
//! enum's values moves. A row is matched on the value SET, so every member targeting one
//! enum — of any operation, at any depth — states that enum's set in the one order, and an
//! enum this service pins no order of is stated as the model declares it.

use service_common::validation_message::{in_stated_enum_order, StatedEnumValueSets};
use smithy_server_runtime::Violation;

/// `TargetTypeEnum`'s values as the MODEL declares them
/// (`smithy_query(query="shape",shape="TargetTypeEnum")`).
const TARGET_TYPE_DECLARED: &[&str] = &["instance", "ip", "lambda", "alb"];

/// The same four values in the order this service STATES them, which is recorded
/// behaviour: a clause rendered from the model alone lists them in the declaration order
/// above.
const TARGET_TYPE_STATED: &[&str] = &["lambda", "ip", "instance", "alb"];

/// Every enum value set this service publishes an order for.
///
/// One row per enum whose stated order is known to differ from the order it reaches a
/// clause in; a value set absent from this table is stated as the model declares it,
/// because this service has shown no other order for it.
const STATED_ENUM_VALUE_SETS: &StatedEnumValueSets<'static> =
    &[(TARGET_TYPE_DECLARED, TARGET_TYPE_STATED)];

/// `violations` with every enum value set stated in the order this service lists it
/// ([`STATED_ENUM_VALUE_SETS`]); every other violation, and every set no order is
/// published for, is passed through unchanged.
pub(crate) fn in_stated_order(violations: &[Violation]) -> Vec<Violation> {
    violations
        .iter()
        .map(|violation| in_stated_enum_order(violation, STATED_ENUM_VALUE_SETS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_server_runtime::ViolationKind;

    /// One enum violation at `path` over `allowed`, as the validating walk reports one.
    fn enum_violation(path: &str, value: &str, allowed: &[&str]) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::EnumValue(allowed.iter().map(|v| (*v).to_string()).collect()),
            value: Some(value.to_string()),
            sensitive: false,
        }
    }

    /// The values an enum violation carries, which is what the clause lists.
    fn stated(violation: &Violation) -> Vec<String> {
        match &violation.kind {
            ViolationKind::EnumValue(values) => values.clone(),
            other => panic!("expected an enum violation, got {other:?}"),
        }
    }

    #[test]
    fn a_published_set_is_stated_in_its_order_whatever_order_it_arrives_in() {
        // The model's declaration order, a sort of it, and the published order itself: one
        // enum, one stated order, whichever order the violation was built from.
        for presented in [
            ["instance", "ip", "lambda", "alb"],
            ["alb", "instance", "ip", "lambda"],
            ["lambda", "ip", "instance", "alb"],
        ] {
            let moved = in_stated_order(&[enum_violation("targetType", "IP", &presented)]);
            assert_eq!(
                stated(&moved[0]),
                TARGET_TYPE_STATED,
                "case presented {presented:?}: unexpected value set"
            );
            assert_eq!(
                (moved[0].path.as_str(), moved[0].value.as_deref()),
                ("targetType", Some("IP")),
                "case presented {presented:?}: the position and the echoed value must be kept"
            );
        }
    }

    #[test]
    fn a_set_no_order_is_published_for_is_stated_as_the_model_declares_it() {
        let declared = ["ipv4", "dualstack"];
        let moved = in_stated_order(&[enum_violation("ipAddressType", "IPV4", &declared)]);
        assert_eq!(
            stated(&moved[0]),
            declared,
            "case an unpublished value set: it must be stated as presented"
        );
    }

    #[test]
    fn a_violation_of_another_class_is_passed_through() {
        let violation = Violation {
            path: "port".to_string(),
            kind: ViolationKind::MaxValue("65535".to_string()),
            value: Some("70000".to_string()),
            sensitive: false,
        };
        assert_eq!(
            in_stated_order(std::slice::from_ref(&violation)),
            vec![violation.clone()],
            "case a range violation: it must be passed through unchanged"
        );
    }

    #[test]
    fn every_violation_of_a_request_is_carried_through_in_order() {
        let first = enum_violation("targetType", "IP", TARGET_TYPE_DECLARED);
        let second = Violation {
            path: "port".to_string(),
            kind: ViolationKind::MinValue("1".to_string()),
            value: Some("0".to_string()),
            sensitive: false,
        };
        let moved = in_stated_order(&[first, second.clone()]);
        assert_eq!(
            moved.len(),
            2,
            "case two violations: both must come back, got {moved:?}"
        );
        assert_eq!(
            stated(&moved[0]),
            TARGET_TYPE_STATED,
            "case two violations: the enum's set must be the stated one"
        );
        assert_eq!(
            moved[1], second,
            "case two violations: the other must be unchanged"
        );
    }
}
