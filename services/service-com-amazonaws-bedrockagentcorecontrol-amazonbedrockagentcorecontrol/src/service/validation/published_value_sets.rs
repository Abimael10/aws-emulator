// SPDX-License-Identifier: CC0-1.0

//! The values a clause ENUMERATES for an enumerated type this endpoint publishes a wider
//! set for than its on-disk model declares, as rule 7 of [`super`] states it.
//!
//! [`super`] owns the sentence — which clauses a refusal states, the member each names and
//! the requirement each states — and this module owns one input to it: the value set an
//! enum clause lists. A shipped model is a SNAPSHOT of the service, and a service that has
//! grown a value since that snapshot still enumerates the grown set when it refuses a value
//! outside the type. A caller corrects its request from that enumeration, so a clause that
//! lists fewer values than the service accepts sends it to a vendor the service does not
//! have.
//!
//! WHAT IS AND IS NOT DECIDED HERE: only the SENTENCE. Which values a member ADMITS is the
//! model's, walked by the framework, so a value this module lists and the shipped enum does
//! not is still refused — the published set is what the refusal NAMES, never what it
//! accepts. Each set is the service's own datum and therefore lives beside the resource it
//! belongs to in [`crate::state`]; this module only reaches for it.
//!
//! KEYED BY THE MEMBER THAT TARGETS THE TYPE, not by the set a violation reports: two
//! enumerated types of this model are declared with the SAME values
//! (`PaymentCredentialProviderVendorType` and `PaymentConnectorType`, both
//! `[CoinbaseCDP, StripePrivy]`), so the reported set cannot tell them apart, while the
//! member a value arrived at can. Every member targeting a listed type is listed, so one
//! type is enumerated alike in every operation that carries it rather than only in the one
//! a report happened to cite.

use std::collections::HashMap;
use std::sync::LazyLock;

use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::{self, EnumTrait, Violation, ViolationKind};

use super::constraint_trait;

/// `violation` with an enumerated type's value set listed as this endpoint publishes it,
/// and every other violation — and every enum this module publishes no set for — returned
/// unchanged.
///
/// The CLAUSE stays [`super`]'s: only the values that go into it move, so the member path,
/// the echoed value and the requirement's wording keep coming from the one place that
/// writes them.
pub(super) fn restated(violation: &Violation) -> Violation {
    let mut restated = violation.clone();
    if matches!(violation.kind, ViolationKind::EnumValue(_)) {
        if let Some(published) = PUBLISHED.get(violation.path.as_str()) {
            restated.kind = ViolationKind::EnumValue(
                published.iter().map(|value| (*value).to_string()).collect(),
            );
        }
    }
    restated
}

/// The published value set of every member that targets a type this endpoint publishes one
/// for, keyed by the member path the framework's walk reports a violation at.
///
/// Built once off the generated schemas: a member [`PUBLISHED_MEMBERS`] names and the model
/// attaches no enumerated set to is left OUT, so a member renamed or retargeted upstream
/// cannot silently take a published set with it — the framework's own clause stands
/// instead, which is the fail-safe direction.
static PUBLISHED: LazyLock<HashMap<String, &'static [&'static str]>> = LazyLock::new(|| {
    PUBLISHED_MEMBERS
        .iter()
        .filter(|(schema, member_name, _)| declared_values(schema, member_name).is_some())
        .map(|(_, member_name, published)| (constraints::path::member(member_name), *published))
        .collect()
});

/// Every member of this model that targets an enumerated type this endpoint publishes a
/// value set of its own for: the shape that declares the member, the member's name as the
/// wire spells it, and the values published for the type it targets.
///
/// The shape is named because the declared set is reachable only THROUGH a member: codegen
/// attaches the target type's constraint to each member that targets it, and a member schema
/// carries no link to the schema of the shape it targets.
///
/// Source for the member list of `PaymentCredentialProviderVendorType`:
/// `smithy_query(query="shape",shape="CreatePaymentCredentialProviderRequest,\
/// UpdatePaymentCredentialProviderRequest")` — the two requests of this model that carry
/// `credentialProviderVendor`, which is the only member targeting that type.
static PUBLISHED_MEMBERS: &[(&Schema, &str, &[&str])] = &[
    (
        crate::ops::types::CREATE_PAYMENT_CREDENTIAL_PROVIDER_INPUT_SCHEMA,
        "credentialProviderVendor",
        crate::state::PUBLISHED_PAYMENT_VENDORS,
    ),
    (
        crate::ops::types::UPDATE_PAYMENT_CREDENTIAL_PROVIDER_INPUT_SCHEMA,
        "credentialProviderVendor",
        crate::state::PUBLISHED_PAYMENT_VENDORS,
    ),
];

/// The value set the member `member_name` of `schema` admits, as the MODEL's own schema
/// carries it, or `None` when the model declares no such member or attaches no enumerated
/// set to it.
fn declared_values(schema: &'static Schema, member_name: &str) -> Option<&'static [&'static str]> {
    let member = schema
        .members()
        .iter()
        .copied()
        .find(|member| member.member_name() == Some(member_name))?;
    Some(constraint_trait::<EnumTrait>(member.traits()?, &EnumTrait::TRAIT_ID)?.values)
}

#[cfg(test)]
mod tests {
    //! What a published set has to be true of, read off the model rather than off a
    //! request. The end-to-end half — which operation enumerates which set — is
    //! `tests/all/payment_vendor_value_set.rs`.

    use super::*;

    /// One violation at `path` over the value set `admitted`, as the validating walk reports
    /// one.
    fn enum_violation(path: &str, admitted: &[&str]) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::EnumValue(
                admitted.iter().map(|value| (*value).to_string()).collect(),
            ),
            value: Some("NotARealPaymentVendor".to_string()),
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

    /// A published set never NARROWS the model: every value the shipped enum admits is one
    /// the clause names, so a value a caller may legally send is never left out of the
    /// enumeration it reads.
    #[test]
    fn a_published_set_names_every_value_the_model_declares() {
        for (schema, member_name, published) in PUBLISHED_MEMBERS {
            let declared = declared_values(schema, member_name)
                .expect("the model declares the member and its value set");
            for value in declared {
                assert!(
                    published.contains(value),
                    "case {member_name:?}: the published set {published:?} must name every \
                     declared value, and omits {value:?}"
                );
            }
        }
    }

    /// Every member that targets a published type reaches the catalog, whichever operation
    /// declares it: the set belongs to the type, not to the operation that carried the
    /// value.
    #[test]
    fn every_published_member_states_the_published_set() {
        for (schema, member_name, published) in PUBLISHED_MEMBERS {
            let path = constraints::path::member(member_name);
            let declared = declared_values(schema, member_name)
                .expect("the model declares the member and its value set");
            let moved = restated(&enum_violation(&path, declared));
            let expected: Vec<String> =
                published.iter().map(|value| (*value).to_string()).collect();
            assert_eq!(
                listed(&moved),
                expected,
                "case {member_name:?}: expected the published set {published:?}, got {:?}",
                listed(&moved)
            );
            assert_eq!(
                (moved.path.as_str(), moved.value.as_deref()),
                (path.as_str(), Some("NotARealPaymentVendor")),
                "case {member_name:?}: the member and the echoed value must be kept"
            );
        }
    }

    /// A violation at any other member, and of any other class, is passed through: only a
    /// published type's enumeration moves.
    #[test]
    fn every_other_violation_is_passed_through() {
        let rows = [
            (
                // `PaymentConnectorType`'s member, declared with the SAME values as the
                // vendor type and published for by nothing here.
                "another enumerated type declared with the same values",
                enum_violation("type", &["CoinbaseCDP", "StripePrivy"]),
            ),
            (
                "an enum at a member no published type reaches",
                enum_violation("credentialProviderVendorType", &["CoinbaseCDP"]),
            ),
            (
                "a @pattern at a published member",
                Violation {
                    path: "credentialProviderVendor".to_string(),
                    kind: ViolationKind::Pattern("^[a-zA-Z0-9]+$".to_string()),
                    value: Some("not a vendor".to_string()),
                    sensitive: false,
                },
            ),
            (
                "an absent required member",
                Violation {
                    path: "credentialProviderVendor".to_string(),
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    sensitive: false,
                },
            ),
        ];
        for (case, violation) in rows {
            let moved = restated(&violation);
            assert_eq!(
                moved, violation,
                "case {case:?}: expected the violation passed through unchanged, got {moved:?}"
            );
        }
    }
}
