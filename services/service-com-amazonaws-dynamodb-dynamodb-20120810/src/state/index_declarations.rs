// SPDX-License-Identifier: CC0-1.0

//! How a request's INDEX DECLARATIONS are refused.
//!
//! Two facts about the declaration a request carries rather than about what the store
//! holds, so they live here and not in the operations that read them:
//!
//! * a name declared for two indexes of one table. "The name must be unique among all
//!   other indexes on this table" (`GlobalSecondaryIndex$IndexName`, and
//!   `LocalSecondaryIndex$IndexName` word for word), so a declaration that states the
//!   same name twice names no two indexes and is refused — with a sentence about the
//!   repeated NAME and nothing about which member or which index family it arrived in,
//!   because the fact is the same in all of them ([`duplicate_index_name_message`]);
//! * an entry of an index-update list that asks for no action at all. Each entry of such
//!   a list "represents one of the following" (the `GlobalSecondaryIndexUpdate` doc) — one
//!   action per entry — so an entry carrying none of its action members describes no
//!   change, and the refusal names the member paths one of them has to arrive in
//!   ([`no_index_action_message`], [`one_index_action_message`]).
//!
//! Both sentences open with the clause every refusal of a parameter value in this service
//! carries ([`super::INVALID_PARAMETER_CLAUSE`]). `super` re-exports this module, so every
//! caller still names them `crate::state::…`.

use super::{RequestError, INVALID_PARAMETER_CLAUSE};

/// AWS's text for a request that declares one index name twice.
///
/// Fixed throughout but for the repeated name: the defect is that the DECLARATION names
/// two indexes the same, which is the same defect whichever index lists the two entries
/// arrived in — two global secondary indexes, two local ones, a vector index and a
/// secondary one, or two entries of one update list — so every surface that declares an
/// index draws this one sentence rather than naming its own member.
pub fn duplicate_index_name_message(name: &str) -> String {
    format!("{INVALID_PARAMETER_CLAUSE}: Duplicate index name: {name}")
}

/// Refuse a declaration that states `name` for a second index.
pub fn duplicate_index_name(name: &str) -> RequestError {
    RequestError::Validation(duplicate_index_name_message(name))
}

/// Every action member of one index-update structure, as `<Shape>.<Member>`, in the order
/// the model declares them — which is the order the sentences below name them in.
fn action_paths(shape: &str, actions: &[&str]) -> String {
    actions
        .iter()
        .map(|action| format!("{shape}.{action}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// AWS's text for an entry of an index-update list that carries NONE of its action
/// members: the entry asks for no change, and the sentence names each member path one of
/// them has to arrive in.
pub fn no_index_action_message(shape: &str, actions: &[&str]) -> String {
    format!(
        "{INVALID_PARAMETER_CLAUSE}: One of {} must not be null",
        action_paths(shape, actions)
    )
}

/// The other half of the same rule, for an entry that carries MORE than one of them: one
/// entry is one action ("Represents one of the following" — the `GlobalSecondaryIndexUpdate`
/// doc), so an entry stating two describes no single change the table could apply. The
/// member paths are named exactly as the sentence above names them, since it is the same
/// rule about the same members.
pub fn one_index_action_message(shape: &str, actions: &[&str]) -> String {
    format!(
        "{INVALID_PARAMETER_CLAUSE}: Only one of {} may be specified",
        action_paths(shape, actions)
    )
}
