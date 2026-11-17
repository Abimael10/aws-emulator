// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enumerated type's admissible values in when it refuses
//! a value outside that type.
//!
//! RULE: a refusal of a value outside an enumerated type lists that type's admissible set
//! in the order the front door's own TABLE of those values is walked — the table's buckets
//! in ascending order, each bucket being the low bits of the value's own string hash
//! ([`::service_common::validation_message::in_hashed_enum_order`], this tree's one home
//! for that derivation) — and not in the order the model declares the type's members in.
//! The order is COMPUTED from the values themselves, so it belongs to the enumerated TYPE
//! and is wired in for EVERY enum of this model: every member targeting one enum is listed
//! alike, in any operation, at any depth, and a type the model gains needs nothing added
//! here.
//!
//! Only the ORDER moves. The counted envelope, the clause, the member path, the value the
//! clause echoes and every other constraint class's requirement stay the framework's —
//! [`::aws_smithy_schema_server::envelope`] words them, exactly as it does for a refusal
//! this service does not restate — so this service cannot word a constraint differently
//! from the one that renders it; a refusal carrying no enum clause comes back untouched.

use ::service_common::validation_message::in_hashed_enum_order;
use ::smithy_server_runtime::Violation;

/// The violations of ONE refusal as this service states them: an enum value set listed in
/// the order the table of the admissible values is walked, and every other violation kind
/// unchanged.
pub(crate) fn as_this_service_states(violations: &[Violation]) -> Vec<Violation> {
    violations.iter().map(in_hashed_enum_order).collect()
}

/// The request-validation envelope over `violations`, with every enum value set in it
/// listed in the order this service states that set ([`as_this_service_states`]).
///
/// `None` when there is nothing to state, which is the framework's own answer for an empty
/// refusal.
pub(crate) fn validation_envelope(violations: &[Violation]) -> Option<String> {
    ::aws_smithy_schema_server::envelope(&as_this_service_states(violations))
}
