// SPDX-License-Identifier: CC0-1.0

//! WHICH values this service's front door states that an enumerated type admits, and
//! in which order.
//!
//! A value outside an enumerated type is refused with a clause naming the values that
//! type admits. Two things about that listing belong to the front door rather than to
//! the model:
//!
//! * its MEMBERSHIP. The front door answers from its own table of admissible values for
//!   the type, and that table can hold FEWER values than the model's enum declares —
//!   `KnowledgeBaseType` is such a type here: the model declares four kinds of knowledge
//!   base, while a refusal of a `knowledgeBaseConfiguration.type` outside the set lists
//!   the self-managed vector kind alone. So the clause advertises the values the front
//!   door publishes for the type, not the values the model declares.
//! * its ORDER. That table is keyed on each value's own text, and the front door walks
//!   the table rather than the declaration
//!   (`service_common::validation_message::hashed_enum_order`), so the listing is the
//!   table's buckets in ascending order.
//!
//! Both are properties of the TYPE, derived from the values themselves, so one type is
//! listed alike wherever it is carried — whichever member, operation, depth or binding
//! brought the value in — and a type this model gains needs nothing added here.
//!
//! The published set decides only what a refusal LISTS. WHICH values a member admits
//! stays the enum the member targets, so every kind of knowledge base the model declares
//! is still created, read and updated: nothing here refuses a value, and nothing here is
//! asked unless the request already broke a constraint.
//!
//! [`in_front_door_enum_value_set`] is how the service's refusal seam
//! (`crate::service`) hands a boundary violation through both steps, and
//! [`stated_value_set`] how a HANDLER that judges a discriminator the boundary cannot
//! states the same clause — so the two cannot drift apart.

use super::*;

use ::aws_smithy_schema_server::Violation;
use ::service_common::validation_message::{
    hashed_enum_order, in_hashed_enum_order, in_stated_enum_order, stated_enum_value_set,
    StatedEnumValueSets,
};

/// The `KnowledgeBaseType` values the MODEL declares, read off the one table of them
/// this crate holds ([`KNOWLEDGE_BASE_CONFIGURATION_MEMBERS`]) so the set is written
/// once.
const DECLARED_KNOWLEDGE_BASE_TYPES: [&str; 4] = [
    KNOWLEDGE_BASE_CONFIGURATION_MEMBERS[0].0,
    KNOWLEDGE_BASE_CONFIGURATION_MEMBERS[1].0,
    KNOWLEDGE_BASE_CONFIGURATION_MEMBERS[2].0,
    KNOWLEDGE_BASE_CONFIGURATION_MEMBERS[3].0,
];

// The row above is matched as a SET, so it has to hold EVERY value the model declares: a
// model revision that adds a kind must extend it rather than leave it short, which would
// quietly stop the row matching and send the clause back to listing the declaration.
const _: () = assert!(
    DECLARED_KNOWLEDGE_BASE_TYPES.len() == KNOWLEDGE_BASE_CONFIGURATION_MEMBERS.len(),
    "every KnowledgeBaseType the model declares belongs in DECLARED_KNOWLEDGE_BASE_TYPES"
);

/// The `KnowledgeBaseType` values the front door PUBLISHES: the self-managed vector
/// kind alone, which is what a recorded refusal of a `knowledgeBaseConfiguration.type`
/// outside the set lists. The other three kinds the model declares are not in the
/// table the front door answers from, so they are not advertised by a refusal.
const PUBLISHED_KNOWLEDGE_BASE_TYPES: [&str; 1] = [VECTOR_KNOWLEDGE_BASE_TYPE];

/// Every enumerated type of this model whose published set is not its declared one: the
/// values as the MODEL declares them, paired with the values the FRONT DOOR publishes.
///
/// A type with no row here is published exactly as the model declares it, which is the
/// case for every other enumerated type of this model.
const PUBLISHED_ENUM_VALUE_SETS: &StatedEnumValueSets<'static> = &[(
    &DECLARED_KNOWLEDGE_BASE_TYPES,
    &PUBLISHED_KNOWLEDGE_BASE_TYPES,
)];

/// The values a clause lists for the enumerated type whose admissible values are
/// `admitted`: the set the front door publishes for that type, in the order it walks
/// its table of them.
///
/// `admitted` is matched as a SET, so the order it is presented in — the model's
/// declaration, or whatever order a caller held it in — does not decide the listing.
#[must_use]
pub fn front_door_enum_value_set(admitted: &[&str]) -> Vec<String> {
    let presented: Vec<String> = admitted.iter().map(|value| (*value).to_string()).collect();
    let published = stated_enum_value_set(&presented, PUBLISHED_ENUM_VALUE_SETS);
    match published {
        Some(published) => hashed_enum_order(published)
            .into_iter()
            .map(str::to_string)
            .collect(),
        None => hashed_enum_order(admitted)
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

/// The values of an enumerated type as this service LISTS them inside a
/// `Member must satisfy enum value set: [ … ]` clause
/// ([`front_door_enum_value_set`], comma-separated).
///
/// A handler that judges a discriminator the request boundary cannot — a `type` that
/// names which sibling member carries its details — states the same clause the boundary
/// states, so it lists the set the same way.
#[must_use]
pub(crate) fn stated_value_set(admitted: &[&str]) -> String {
    front_door_enum_value_set(admitted).join(", ")
}

/// `violation` with an enumerated type's value set stated as this front door states it,
/// and any other violation returned unchanged.
///
/// Both steps of the listing in one place: the published set for the type
/// ([`PUBLISHED_ENUM_VALUE_SETS`]), then the order the table of those values is walked.
/// The CLAUSE stays the framework's — only the values move, so the member path, the
/// echoed value and every requirement's wording keep coming from the one place that
/// writes them.
#[must_use]
pub fn in_front_door_enum_value_set(violation: &Violation) -> Violation {
    let published = in_stated_enum_order(violation, PUBLISHED_ENUM_VALUE_SETS);
    in_hashed_enum_order(&published)
}
