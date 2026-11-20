// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enumerated type's admissible values in when the boundary
//! refuses a value outside that type.
//!
//! RULE: a refusal of a value outside an enumerated type lists that type's admissible set in
//! the order the front door's own TABLE of those values is walked — the table's buckets in
//! ascending order, each bucket being the low bits of the value's own string hash
//! ([`::service_common::validation_message::in_hashed_enum_order`], this tree's one home for
//! that derivation) — and not in the order the model declares the type's members in.
//!
//! The order is COMPUTED from the values themselves, so it belongs to the enumerated TYPE
//! and is wired in for EVERY enum of this model: every member targeting one enum is listed
//! alike, in any operation, at any depth, and a type the model gains needs nothing added
//! here.
//!
//! Only the ORDER moves. The counted envelope, the clause, the member path and the value the
//! clause echoes are the framework's own (`aws_smithy_schema_server::envelope`), so this
//! service cannot word a constraint differently from the services that state the same
//! sentence; a refusal carrying no enum clause comes back untouched.
//!
//! [`stated_message`] is the whole sentence, and the ONE home for it: both layers that word
//! a constraint refusal of this service — the service's own seam for its main endpoint and
//! the generic validation in front of its data endpoint ([`super::data_endpoint`]) — state
//! it from here, and differ in the error CODE alone.

use ::service_common::validation_message::in_hashed_enum_order;
use ::smithy_server_runtime::Violation;

/// Every violation as THIS SERVICE states it: an enum value set listed in the order the
/// table of the admissible values is walked, and every other violation kind unchanged.
pub(super) fn as_this_service_states(violations: &[Violation]) -> Vec<Violation> {
    violations.iter().map(in_hashed_enum_order).collect()
}

/// The WORDS this service refuses `violations` with: the framework's own counted envelope
/// over its own clauses, each enum value set listed as above. `None` when there is nothing
/// to state.
///
/// The one home for them, so the layer that words a refusal for the service's main
/// endpoint ([`super::EmulatedService::override_refusal`]) and the one that words it for
/// the data endpoint ([`super::data_endpoint`]) cannot state the same constraint
/// differently: the two differ in the error CODE they travel under and in nothing else.
pub(super) fn stated_message(violations: &[Violation]) -> Option<String> {
    ::aws_smithy_schema_server::envelope(&as_this_service_states(violations))
}
