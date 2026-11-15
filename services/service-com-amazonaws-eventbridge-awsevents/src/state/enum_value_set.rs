// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enumerated type's admissible values in when it refuses
//! a value outside that type, for a caller holding the VALUES rather than a violation.
//!
//! RULE: the admissible set is listed in the order the front door's own TABLE of those
//! values is walked — the table's buckets in ascending order, each bucket being the low
//! bits of the value's own string hash
//! ([`::service_common::validation_message::hashed_enum_order`], this tree's one home for
//! that derivation) — and not in the order the model declares the type's members in. The
//! order is COMPUTED from the values themselves, so it belongs to the enumerated TYPE:
//! every member targeting one enum is listed alike, in any operation, at any depth, and a
//! type the model gains needs nothing added here.
//!
//! This is the HANDLER's door onto that order, beside the other refusal wording a handler
//! enforcing an unmodelled constraint reaches ([`super::constraint_clause`],
//! [`super::validation_envelope`]); the boundary restates the violations the framework
//! found through `crate::service::enum_value_sets`, which applies the same shared
//! derivation. Nothing about the SENTENCE is this module's: the requirement is taken out of
//! the framework's own clause
//! ([`::service_common::validation_message::requirement`]), so a refusal a handler words
//! cannot read differently from one the boundary words.

use ::service_common::validation_message::{hashed_enum_order, requirement};
use ::smithy_server_runtime::ViolationKind;

/// The requirement half of the clause a value outside an enumerated type earns: the whole
/// admissible set, listed in the order this service lists that type's values in.
///
/// `declared` is the type's admissible values as the caller holds them — a generated
/// `…_VALUES` list in a handler, say — in any order: the listing order is derived from the
/// values, so the order they arrive in does not change it, except among values that share
/// a bucket, where the order they were declared in decides (see the shared derivation). A
/// handler holding a list in another order than the model declares (the generated lists are
/// alphabetical) therefore states the same set as the boundary for every type whose values
/// sit in distinct buckets, and should pass the declared order where two of them collide.
pub fn requirement_over<S: AsRef<str>>(declared: &[S]) -> String {
    requirement(ViolationKind::EnumValue(
        hashed_enum_order(declared)
            .into_iter()
            .map(str::to_string)
            .collect(),
    ))
}
