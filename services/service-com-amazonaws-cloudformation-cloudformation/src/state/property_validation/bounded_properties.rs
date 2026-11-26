// SPDX-License-Identifier: CC0-1.0

//! The other thing a resource type's registry SCHEMA makes decidable about a property's value:
//! how long it may be, stated as that property's `minLength` and `maxLength`.
//!
//! A schema states, beside the spelling a property admits ([`super::patterned_properties`]) and
//! the values it publishes ([`super::enum_properties`]), the LENGTH bounds a value of it has to
//! be inside — the bounds the member the property is passed to publishes, so a value outside
//! them is one no handler of that type could ever be given. The stage that runs before
//! provisioning reports it and the operation deploys nothing.
//!
//! The bounds are judged BEFORE the pattern, and that order is observable: a value that is both
//! too short and outside its property's pattern is reported as the LENGTH failure, which is the
//! verdict a schema validator reports for it. A value that is not TEXT has no length to judge
//! and is admitted, exactly as it is for the spelling question.
//!
//! What the validator reports is the JSON POINTER of the judged value inside the property
//! document, the bound it was held to and the length it had ([`length_failure_reason`]) — and
//! nothing of the value itself, so a property whose value is a credential needs no redaction
//! here.

use super::{PropertyFailure, StoredResource};

/// One property whose length bounds its type's schema states.
#[derive(Debug, Clone, Copy)]
pub struct BoundedProperty {
    /// The resource type the property belongs to, as a template declares it.
    pub resource_type: &'static str,
    /// The chain of members leading from the resource's property document down to the property
    /// the schema states the bounds on, each spelled as a template spells it — the same reading
    /// the spelling question takes ([`super::patterned_properties::PatternedProperty::property`]).
    pub property: &'static [&'static str],
    /// The `minLength` its type's schema states for the property, where it states one.
    pub min_length: Option<usize>,
    /// The `maxLength` it states, where it states one.
    pub max_length: Option<usize>,
}

/// The properties this stage knows the published length bounds of.
///
/// Source for each row: the `minLength`/`maxLength` the resource type's registry schema states
/// for that property, which are the `@length` bounds the OWNING API publishes for the member the
/// property is passed to — so a value outside them is one that service would refuse and the
/// validation refuses it first.
pub const BOUNDED_PROPERTIES: &[BoundedProperty] = &[
    // `AWS::AIOps::InvestigationGroup`'s `Name` is `CreateInvestigationGroup$name`, "a name for
    // the investigation group", whose published bounds are `@length 1..512`; its `RoleArn` is
    // that call's `roleArn`, "the ARN of the IAM role that CloudWatch investigations will use
    // when it gathers investigation data", whose published bounds are `@length 20..2048`
    // (`smithy_query(query="operation",operation="CreateInvestigationGroup")` of the
    // `com-amazonaws-aiops-aiops` model) — which this type's schema states as those two members'
    // `minLength`/`maxLength`.
    BoundedProperty {
        resource_type: crate::state::template::AIOPS_INVESTIGATION_GROUP,
        property: &["Name"],
        min_length: Some(1),
        max_length: Some(512),
    },
    BoundedProperty {
        resource_type: crate::state::template::AIOPS_INVESTIGATION_GROUP,
        property: &["RoleArn"],
        min_length: Some(20),
        max_length: Some(2048),
    },
];

/// `Some(failure)` for a resource carrying a property value outside the length bounds its type's
/// schema states for that property ([`BOUNDED_PROPERTIES`]) — the first such property in the
/// order the catalogue states them, and within a list-valued one the first element outside the
/// bounds; `None` for a type with no row, for a property the declaration does not carry, for a
/// value that is not text, and for every value inside its bounds.
pub(super) fn value_outside_its_length(resource: &StoredResource) -> Option<PropertyFailure> {
    BOUNDED_PROPERTIES
        .iter()
        .filter(|row| row.resource_type == resource.resource_type)
        .find_map(|row| {
            let (at, bound, length) =
                super::patterned_properties::judged_values(&resource.properties, row.property)
                    .into_iter()
                    .find_map(|(at, declared)| {
                        exceeded_bound(row, declared).map(|(bound, length)| (at, bound, length))
                    })?;
            let at: Vec<&str> = at.iter().map(String::as_str).collect();
            Some(PropertyFailure {
                logical_id: resource.logical_id.clone(),
                resource_type: resource.resource_type.clone(),
                reason: length_failure_reason(&at, bound, length),
                path: super::member_path(&resource.logical_id, &at),
            })
        })
}

/// Which bound `declared` is outside, and how long it is — `None` for a value inside both.
///
/// The length is counted in CHARACTERS, which is the unit a schema's `minLength`/`maxLength`
/// bounds a string in. The MINIMUM is judged first: a value can only be outside one of the two,
/// and reporting the lower bound first is what makes a value stated as nothing report the bound
/// it has to reach rather than the one it is nowhere near.
fn exceeded_bound(row: &BoundedProperty, declared: &str) -> Option<(Bound, usize)> {
    let length = declared.chars().count();
    if let Some(minimum) = row.min_length {
        if length < minimum {
            return Some((Bound::Minimum(minimum), length));
        }
    }
    if let Some(maximum) = row.max_length {
        if length > maximum {
            return Some((Bound::Maximum(maximum), length));
        }
    }
    None
}

/// Which of a property's two length bounds a value is outside, with the bound's own value — the
/// one thing the two halves of a length failure differ in.
#[derive(Debug, Clone, Copy)]
pub enum Bound {
    /// The `minLength` the schema states, which the value is shorter than.
    Minimum(usize),
    /// The `maxLength` it states, which the value is longer than.
    Maximum(usize),
}

impl Bound {
    /// The keyword the schema states this bound under, which is the keyword its validator names
    /// in the sentence it reports.
    fn keyword(self) -> &'static str {
        match self {
            Self::Minimum(_) => "minLength",
            Self::Maximum(_) => "maxLength",
        }
    }

    /// The bound's own value, as the sentence states what was expected.
    fn value(self) -> usize {
        match self {
            Self::Minimum(bound) | Self::Maximum(bound) => bound,
        }
    }
}

/// How the schema validator reports a property value outside the length bounds its property's
/// schema states: the JSON POINTER of the judged value inside the property document, then the
/// bound it was held to and the length it had.
///
/// Every variable part is what was judged — the pointer is the member chain the value sits at
/// (the schema's own `#/`-rooted form, which is the document the validator ran on), the keyword
/// and its value the bound the property publishes, and the length the value's own. The VALUE
/// itself is not named, which is why a property carrying a credential needs no redaction here.
pub fn length_failure_reason(at: &[&str], bound: Bound, length: usize) -> String {
    format!(
        "#/{}: expected {}: {}, actual: {length}",
        at.join("/"),
        bound.keyword(),
        bound.value()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::serde_json::{json, Value};
    use ::std::collections::BTreeMap;

    /// One declared resource of `resource_type` carrying `properties`.
    fn declared(logical_id: &str, resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: logical_id.to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: format!("{logical_id}-physical"),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// RULE: a value shorter than its property's `minLength` or longer than its `maxLength` is
    /// reported with the bound it was held to and the length it had, and a value at either bound
    /// passes — so every row is exercised on each published bound and one past it.
    #[test]
    fn a_value_outside_its_published_bounds_is_reported_with_that_bound() {
        for row in BOUNDED_PROPERTIES {
            let property = row.property.last().copied().unwrap_or_default();
            // (case, how long the value is, the bound it is outside — `None` where it is inside
            // both). Built from the bounds the row actually states, so a row stating only one of
            // them is probed on that one alone.
            let mut cases: Vec<(&str, usize, Option<Bound>)> = Vec::new();
            if let Some(minimum) = row.min_length {
                cases.push(("at the lower bound", minimum, None));
                if minimum > 0 {
                    cases.push((
                        "one character short of the lower bound",
                        minimum - 1,
                        Some(Bound::Minimum(minimum)),
                    ));
                }
            }
            if let Some(maximum) = row.max_length {
                cases.push(("at the upper bound", maximum, None));
                cases.push((
                    "one character past the upper bound",
                    maximum + 1,
                    Some(Bound::Maximum(maximum)),
                ));
            }
            for (case, length, outside) in cases {
                let resource =
                    declared("R", row.resource_type, json!({ property: "a".repeat(length) }));
                let failed = value_outside_its_length(&resource);
                match outside {
                    None => assert!(
                        failed.is_none(),
                        "case {property} {case:?}: expected a value inside the bounds to pass, \
                         got {failed:?}"
                    ),
                    Some(bound) => {
                        let failure = failed.unwrap_or_else(|| {
                            panic!(
                                "case {property} {case:?}: expected the bound to be reported, \
                                 got none"
                            )
                        });
                        assert_eq!(
                            failure.reason,
                            length_failure_reason(row.property, bound, length),
                            "case {property} {case:?}: expected the bound and the length to be \
                             reported, got {failure:?}"
                        );
                        assert_eq!(
                            failure.path,
                            super::super::member_path("R", row.property),
                            "case {property} {case:?}: expected the property's own path, got \
                             {failure:?}"
                        );
                    }
                }
            }
        }
    }

    /// RULE: a value that is not TEXT has no length this stage judges, so it is admitted and
    /// left to the service that owns the resource.
    #[test]
    fn a_value_that_is_not_text_has_no_length_to_judge() {
        for row in BOUNDED_PROPERTIES {
            let property = row.property.last().copied().unwrap_or_default();
            for value in [json!(7), json!(true), json!({"Ref": "AParameter"}), json!([])] {
                let resource = declared("R", row.resource_type, json!({ property: value }));
                let failed = value_outside_its_length(&resource);
                assert!(
                    failed.is_none(),
                    "case {property} {value:?}: expected a value that is no text to be admitted, \
                     got {failed:?}"
                );
            }
        }
    }
}
