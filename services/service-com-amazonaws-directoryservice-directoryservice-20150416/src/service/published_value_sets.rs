// SPDX-License-Identifier: CC0-1.0

//! The value set a constraint clause of this service LISTS for an enumerated member, and
//! the order it lists that set in.
//!
//! ## The rule
//!
//! A clause about a value outside its member's enumerated type names the values the member
//! may carry (`Member must satisfy enum value set: [<values>]`). Two things about that list
//! belong to the SERVICE rather than to the model:
//!
//! * WHICH values it holds, and how each one is spelled. The list is what the service tells
//!   a caller it accepts, so a type whose live value set has moved past the model shipped
//!   with this emulator is listed as the service publishes it — and a caller reading the
//!   refusal is not told that a value the service names does not exist. Those rows are this
//!   service's own data, so they live here ([`PUBLISHED_VALUE_SETS`]), keyed on the value
//!   SET the model declares and therefore holding for every member targeting that type, in
//!   any operation and at any depth.
//! * The ORDER they are listed in. The front door holds a set's values in a HASH TABLE keyed
//!   on each value's own text and enumerates the table rather than the type's declaration, so
//!   the list is neither the model's declaration order nor a sort of it. That derivation is
//!   `service_common`'s ([`hashed_enum_order`]), shared with every other emulated service
//!   whose recorded refusals list a value set that way, and it is not restated here. The
//!   table is the front door's, so the order belongs to EVERY enumerated type of this model
//!   rather than to the types a set is published for.
//!
//! It is what this service's three recorded enum refusals list: `UpdateType`, declared
//! `[OS, NETWORK, SIZE]`, as `[SIZE, OS, NETWORK]`; `ClientAuthenticationType`, declared
//! `[SmartCard, SmartCardOrPassword]`, as `[SmartCardOrPassword, SmartCard]`; and
//! `DirectoryEdition` as `[Enterprise, EnterpriseHybrid, Standard]`, the two values that
//! share a bucket read from the head each was pushed onto.
//!
//! Only the CLAUSE follows any of this: which values a member ADMITS is the enum it targets,
//! checked by the framework's own constraint walk, so a value this service publishes and the
//! shipped model does not declare is still refused if a request carries it. That gap is the
//! shipped model's staleness against the live service, and closing it would be a change to
//! the model rather than to how a refusal is worded.
//!
//! ## Where it is applied
//!
//! Through [`super::stated_enum_values`], which every path of this service's enum clauses
//! goes through — the boundary refusal [`super::stated_refusal`] words and the nested
//! requirement set a clause states at a list member ([`super::list_element_refusals`]) — so
//! one listing serves every member whatever found it.

use ::service_common::validation_message::{
    hashed_enum_order, stated_enum_value_set, StatedEnumValueSets,
};

/// The values the model this emulator ships declares for the edition of a Managed Microsoft
/// AD directory (`smithy_query(query="shape",shape="DirectoryEdition")`).
///
/// This half of the row is what a refusal is MATCHED on, so it is the model's set as the
/// validating walk carries it.
const DIRECTORY_EDITION_DECLARED: &[&str] = &["Enterprise", "Standard", "Hybrid"];

/// The editions the SERVICE publishes for that type: the same three the model declares, with
/// the hybrid one spelled `EnterpriseHybrid` — the name the service publishes for the value
/// the shipped model still spells `Hybrid`.
///
/// SOURCE: the service's own refusal of a value outside the set, which lists
/// `Enterprise, EnterpriseHybrid, Standard`. The LISTING order is not written out here — the
/// front door's table decides it ([`hashed_enum_order`]), exactly as it does for every other
/// set — so these are the three values in the order the type DECLARES them, the hybrid one
/// renamed in place.
///
/// WRITE THEM IN DECLARATION ORDER. The table order is a stable sort by bucket, and two
/// values that share a bucket are read from the head each was pushed onto — the later-declared
/// one first. `EnterpriseHybrid` and `Standard` share one, so it is this slice's order that
/// puts `EnterpriseHybrid` ahead of `Standard` in the clause: reordering the constant would
/// silently change the refusal a caller reads.
const DIRECTORY_EDITION_PUBLISHED: &[&str] = &["Enterprise", "Standard", "EnterpriseHybrid"];

/// The enumerated types this service publishes a value set of its OWN for: the type's
/// modelled set, and the set a clause lists for it.
///
/// A row is matched on the modelled set rather than on the shape, the operation or the member
/// a refusal came from ([`stated_enum_value_set`]), so one published set serves every member
/// targeting that type. A type with no row here is listed with exactly the values the model
/// declares, in that same table order.
const PUBLISHED_VALUE_SETS: &StatedEnumValueSets<'static> =
    &[(DIRECTORY_EDITION_DECLARED, DIRECTORY_EDITION_PUBLISHED)];

/// `declared` — an enumerated type's modelled values, as the validating walk carries them —
/// as a clause of this service LISTS them: the set this service publishes where it publishes
/// one, in the order the front door's table is walked.
///
/// The two steps are in that order because the published set is a SET: which values are
/// listed is decided first, and the order is then derived from the values that end up being
/// listed — so a published set and a model-declared one are listed by the one derivation.
#[must_use]
pub fn published_value_set(declared: &[String]) -> Vec<String> {
    let published: Vec<&str> = stated_enum_value_set(declared, PUBLISHED_VALUE_SETS)
        .map_or_else(
            || declared.iter().map(String::as_str).collect(),
            <[&str]>::to_vec,
        );
    hashed_enum_order(&published)
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the edition type is listed as the SERVICE publishes it, whatever order the
    /// modelled set reaches the clause in — which pins the one thing the row's own order
    /// decides, the two values that share a bucket.
    #[test]
    fn the_edition_set_is_listed_as_the_service_publishes_it() {
        // (case, the modelled values as they reach the clause)
        let rows = [
            ("the model's declaration order", DIRECTORY_EDITION_DECLARED),
            (
                "the order the generated schema hands them over in",
                &["Enterprise", "Hybrid", "Standard"][..],
            ),
            ("that order reversed", &["Standard", "Hybrid", "Enterprise"][..]),
        ];
        let expected = ["Enterprise", "EnterpriseHybrid", "Standard"];
        for (case, declared) in rows {
            let declared: Vec<String> = declared.iter().map(|value| (*value).to_string()).collect();
            let listed = published_value_set(&declared);
            assert_eq!(
                listed, expected,
                "case {case:?}: expected {expected:?}, got {listed:?}"
            );
        }
    }

    /// RULE: a type this service publishes no set of is listed with exactly the values the
    /// model declares, in that same table order — nothing is added, dropped or respelled.
    #[test]
    fn an_unpublished_type_is_listed_with_the_modelled_values() {
        let declared: Vec<String> = ["SIZE", "OS", "NETWORK"]
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        let listed = published_value_set(&declared);
        assert_eq!(
            listed,
            ["SIZE", "OS", "NETWORK"],
            "case \"an unpublished type\": expected the table's order, got {listed:?}"
        );
        let mut held = listed;
        held.sort_unstable();
        let mut modelled = declared;
        modelled.sort_unstable();
        assert_eq!(
            held, modelled,
            "case \"an unpublished type\": expected exactly the modelled values"
        );
    }
}
