// SPDX-License-Identifier: CC0-1.0

//! How this service states that a PARAMETER its own request code requires is not in the
//! request at all.
//!
//! A fact of the request's own shape rather than of the table and item store, so it lives
//! beside [`super`] rather than inside it; `super` re-exports it, so every caller still
//! names it `crate::state::…`.

/// The sentence a request that carries no value for such a parameter earns.
///
/// Not a clause and not inside the `<N> validation error(s) detected: ` envelope: the
/// absence is found by the service's own request code before any constraint is walked, so
/// the whole message is this one sentence, with the member named as the model spells it on
/// the wire and nothing of the operation that was called.
///
/// Which operations answer an absent member this way, and which keep the validating walk's
/// `Member must not be null` clause, follows the OPERATION served and is recorded in
/// `src/service.rs` beside the other refusal wordings that do.
pub fn missing_parameter_message(member: &str) -> String {
    format!("The parameter '{member}' is required but was not present in the request")
}

/// The member path the key schema of a table declaration is reported under, as the
/// validating walk spells it (lowerCamel).
pub const KEY_SCHEMA_MEMBER_PATH: &str = "keySchema";

/// The other way the same absence is stated: the validating WALK's own clause for a member
/// the request carries no value for, inside the `<N> validation error(s) detected: `
/// envelope that counts it.
///
/// A member the MODEL marks `@required` earns this clause from that walk, before any handler
/// runs; a member the model leaves optional while the service's own request code requires it
/// is found absent by that code instead, and the ones observed to be stated this way rather
/// than with the sentence above are worded here so the two readings of the same absence
/// cannot drift apart. The clause states `Value null` — the absence itself — and names the
/// member by the path that walk reports, which is the path a client reads out of the message.
pub fn absent_member_message(path: &str) -> String {
    format!("1 validation error detected: {}", absent_member_clause(path))
}

/// That clause on its own, outside the envelope that counts it: what a refusal stating
/// SEVERAL violated members carries for the one the request states no value for, beside
/// the clauses of the other members it is counted with.
///
/// The one wording for the absence, so the two readings of it cannot drift apart.
pub fn absent_member_clause(path: &str) -> String {
    format!(
        "Value null at '{path}' failed to satisfy constraint: Member must not be null"
    )
}
