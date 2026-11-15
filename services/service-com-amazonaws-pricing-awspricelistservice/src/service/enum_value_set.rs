// SPDX-License-Identifier: CC0-1.0

//! The value set a constraint refusal of this service STATES for an ENUMERATED member.
//!
//! RULE: a refusal states the set the service itself publishes for that enumerated type —
//! its own membership, in its own fixed order — which is a property of the service rather
//! than of the order its model happens to declare the values in. The set is a per-service
//! constant: nothing in the request and nothing in the store varies it, and an enum is
//! recognised by its value SET rather than by the member that carried the rejected value,
//! so every member targeting one enum, in any operation and at any depth, states that
//! enum's set alike.
//!
//! Only the DATA is this crate's: [`STATED_FILTER_TYPE_VALUES`] and the table it sits in.
//! The restatement itself, the clause and the envelope all come from the shared home for
//! boundary-refusal wording (`service_common::validation_message`) and from the framework
//! underneath it, so this service cannot word a constraint differently from any other.
//!
//! Which values a member ACCEPTS is a different question and stays with the model: the enum
//! a member targets is what admits or refuses a value, exactly as it did before this layer
//! existed, and listing a value here is not what makes it acceptable. The same split
//! already governs how this crate quotes a `@pattern`: the expression is quoted as the
//! service publishes it while the CHECK runs the model's own anchored expression.

use service_common::validation_message::{in_stated_enum_order, StatedEnumValueSets};
use smithy_server_runtime::{Violation, ViolationKind};

use crate::ops::types::FILTER_TYPE_VALUES;

/// The `FilterType` values the Price List service STATES when it refuses a value outside
/// that enum, in the order it states them in.
///
/// OBSERVED: the bracketed list of the `InvalidParameterException` a `GetProducts` filter
/// type outside the enum earns, recorded in the report this layer was written for —
/// `issues/things-that-now-work/20261102T121014Z-pricing-getproducts-400-validation-refusals-carry-no-error.md`,
/// which quotes the whole sentence. It is the service's own published set, so it is neither
/// sorted nor in the model's declaration order, and it is one value WIDER than the set the
/// model file declares for `com.amazonaws.pricing#FilterType` (`TERM_MATCH`, `EQUALS`,
/// `CONTAINS`, `ANY_OF`, `NONE_OF`, as `crate::ops::types::FILTER_TYPE_VALUES` holds them):
/// the service also lists `TERM_CONTAIN`. Stating it changes nothing about what a member
/// admits — the suite checks that a value the model does not declare is still refused.
pub const STATED_FILTER_TYPE_VALUES: [&str; 6] = [
    "TERM_CONTAIN",
    "ANY_OF",
    "TERM_MATCH",
    "NONE_OF",
    "CONTAINS",
    "EQUALS",
];

/// Every enum this service publishes its own statement of the value set for, keyed by the
/// set the MODEL declares for that enum (`crate::ops::types::*_VALUES`).
///
/// One row per enum whose stated set is known to differ from the declared one. An enum
/// absent from this table is stated as the model declares it, because this service has
/// shown no other statement of it.
const STATED_ENUM_VALUE_SETS: &StatedEnumValueSets<'static> =
    &[(FILTER_TYPE_VALUES, &STATED_FILTER_TYPE_VALUES)];

/// One violation as THIS SERVICE states it: an enum value set replaced by the service's own
/// statement of that set.
///
/// Every other violation kind — and an enum this service publishes no statement of — comes
/// back unchanged, which is what the refusal seam reads to decide whether to reword at all.
pub(super) fn as_this_service_states_it(violation: &Violation) -> Violation {
    in_stated_enum_order(violation, STATED_ENUM_VALUE_SETS)
}

/// The whole message this service refuses a value outside an enumerated member's set with:
/// the framework's own envelope over one clause, with the set stated as this service states
/// it ([`as_this_service_states_it`]).
///
/// Reached from a HANDLER that has to word that refusal itself. The framework's boundary
/// refuses such a value before any handler runs, so this is the same sentence the seam above
/// answers — written once here, so the two layers cannot state one requirement two ways.
/// `path` is the member path as the framework's own walk spells one
/// (`aws_smithy_schema_server::constraints::path`, which applies the lowerCamel casing a
/// member path carries), and `declared` the set the model declares for that member.
///
/// INFALLIBLE: a clause always has an envelope to go in, and the one thing this layer exists
/// to prevent is a refusal reaching a client with no text, so the clause itself is stated
/// rather than an absent message where the envelope has nothing to count.
#[must_use]
pub fn enumerated_value_refusal(path: &str, value: &str, declared: &[&str]) -> String {
    let stated = as_this_service_states_it(&Violation {
        path: path.to_string(),
        kind: ViolationKind::EnumValue(
            declared.iter().map(|value| (*value).to_string()).collect(),
        ),
        value: Some(value.to_string()),
        sensitive: false,
    });
    ::aws_smithy_schema_server::constraints::envelope(::std::slice::from_ref(&stated))
        .unwrap_or_else(|| stated.message())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A violation of `kind` about `path`, as the framework's walk builds one.
    fn violation(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: Some("EXACT".to_string()),
            sensitive: false,
        }
    }

    /// The model's `FilterType` set as the framework's walk carries it: the model's own
    /// declared order, which is not the order this service states it in.
    fn modelled_filter_type_set() -> Vec<String> {
        ["TERM_MATCH", "EQUALS", "CONTAINS", "ANY_OF", "NONE_OF"]
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    }

    /// The set this service states, as a violation carries one.
    fn stated_filter_type_set() -> Vec<String> {
        STATED_FILTER_TYPE_VALUES
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    }

    /// RULE: every value this service states is one the model declares a filter type may
    /// take, or a refusal would advertise a value the member itself refuses — except the
    /// one the service publishes beyond its model, which is the whole reason for the row.
    #[test]
    fn the_stated_set_is_the_declared_set_and_the_one_value_the_service_adds() {
        let beyond_the_model: Vec<&&str> = STATED_FILTER_TYPE_VALUES
            .iter()
            .filter(|value| !FILTER_TYPE_VALUES.contains(value))
            .collect();
        assert_eq!(
            beyond_the_model,
            vec![&"TERM_CONTAIN"],
            "expected exactly the one value the service lists beyond its model, got \
             {beyond_the_model:?}"
        );
        for declared in FILTER_TYPE_VALUES {
            assert!(
                STATED_FILTER_TYPE_VALUES.contains(declared),
                "case {declared:?}: a value the model declares must be stated too, got \
                 {STATED_FILTER_TYPE_VALUES:?}"
            );
        }
    }

    /// RULE: a refusal of a value outside the enumerated set this service states quotes the
    /// service's own set, at whatever member path the walk found it — one row per form the
    /// path can take (a bare member, an element of a list, a deeper element).
    #[test]
    fn an_enumerated_set_is_quoted_as_this_service_states_it() {
        for path in ["type", "filters.1.member.type", "filters.50.member.type"] {
            let stated = as_this_service_states_it(&violation(
                path,
                ViolationKind::EnumValue(modelled_filter_type_set()),
            ));
            let ViolationKind::EnumValue(quoted) = &stated.kind else {
                panic!("case {path:?}: expected an enumerated violation, got {stated:?}");
            };
            assert_eq!(
                quoted.as_slice(),
                stated_filter_type_set().as_slice(),
                "case {path:?}: expected the set this service states, got {quoted:?}"
            );
            assert_eq!(
                (stated.path.as_str(), stated.value.as_deref()),
                (path, Some("EXACT")),
                "case {path:?}: the path and the value half must survive, got {stated:?}"
            );
        }
    }

    /// RULE: every other violation is stated exactly as the framework built it — an
    /// enumerated set this service publishes no statement of, a shorter or longer one, and
    /// each of the other constraint classes.
    #[test]
    fn a_violation_this_rule_does_not_own_is_stated_as_the_framework_built_it() {
        // (case, the violation's kind)
        let rows: Vec<(&str, ViolationKind)> = vec![
            (
                "another service's enumerated set",
                ViolationKind::EnumValue(vec!["json".to_string(), "csv".to_string()]),
            ),
            (
                "a subset of this set",
                ViolationKind::EnumValue(vec!["TERM_MATCH".to_string()]),
            ),
            (
                "a superset of this set",
                ViolationKind::EnumValue(
                    ["TERM_MATCH", "EQUALS", "CONTAINS", "ANY_OF", "NONE_OF", "OTHER"]
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect(),
                ),
            ),
            ("an absent required member", ViolationKind::MissingRequiredMember),
            ("a value below a length", ViolationKind::MinLength(1)),
            ("a value above a length", ViolationKind::MaxLength(32)),
            ("a value below a range", ViolationKind::MinValue("1".to_string())),
            ("a value above a range", ViolationKind::MaxValue("100".to_string())),
            (
                "a value outside a pattern",
                ViolationKind::Pattern("^[A-Z]{3}$".to_string()),
            ),
        ];

        for (case, kind) in rows {
            let built = violation("filters.1.member.type", kind);
            let stated = as_this_service_states_it(&built);
            assert_eq!(
                stated, built,
                "case {case:?}: expected the violation untouched, got {stated:?}"
            );
        }
    }
}
