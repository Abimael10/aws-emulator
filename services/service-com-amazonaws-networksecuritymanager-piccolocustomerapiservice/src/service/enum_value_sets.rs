// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enumerated type's admissible values in when the
//! boundary refuses a value outside that type, and the RENDERING of a `@pattern` it quotes.
//!
//! RULE: a refusal of a value outside an enumerated type lists that type's admissible set
//! in the order the front door's own TABLE of those values is walked — the table's
//! buckets in ascending order, each bucket being the low bits of the value's own string
//! hash ([`::service_common::validation_message::in_hashed_enum_order`], this tree's one
//! home for that derivation) — and not in the order the model declares the type's members
//! in.
//!
//! The order is COMPUTED from the values themselves, so it belongs to the enumerated TYPE
//! and is wired in for EVERY enum of this model: every member targeting one enum is
//! listed alike, in any operation, in whatever binding carried the value, at any depth,
//! and a type the model gains needs nothing added here.
//!
//! Only the ORDER is this service's own. Everything else — the counted envelope, the
//! clause, the member path, the value the clause echoes and every other constraint class's
//! requirement — comes from
//! [`::service_common::validation_message::aws_validation_refusal`], the fleet's one home
//! for how AWS states a boundary refusal, so this service cannot word a constraint
//! differently from the services that state the same sentence. Two renderings are that
//! home's rather than the framework's, and both now reach this service: a `@pattern`
//! requirement is quoted WITHOUT the whole-value anchors the model wraps it in
//! (`::service_common::validation::unanchored`), and a COLLECTION that broke its `@length`
//! is stated with no value at all. The CHECK is always the model's pattern as written;
//! only the quoting changes.

use ::service_common::validation_message::{aws_validation_refusal, in_hashed_enum_order};
use ::smithy_server_runtime::{Refusal, Refused};

/// How this service states `refusal`, or `None` to keep the framework's wording.
///
/// `None` for anything that is not an input-constraint refusal: an authentication failure
/// is a different fault, with its own code and status, and nothing about it is this
/// service's to word.
pub(super) fn refused(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    // Each violation is re-ordered BEFORE the envelope words it: only the values of an
    // enum clause move, and every other violation class comes back untouched.
    let stated = Refusal::Invalid(violations.iter().map(in_hashed_enum_order).collect());
    aws_validation_refusal(&stated)
}
