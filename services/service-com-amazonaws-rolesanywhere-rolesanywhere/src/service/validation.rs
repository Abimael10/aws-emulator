// SPDX-License-Identifier: CC0-1.0

//! How this service words a request its own modelled constraints refuse, before any
//! handler runs.
//!
//! The envelope is AWS's and the framework's — `N validation error(s) detected: Value
//! '<value>' at '<member path>' failed to satisfy constraint: <requirement>`, rendered by
//! [`::service_common::validation_message::aws_validation_refusal`] — and TWO renderings
//! inside it are this service's, both of them properties of the SHAPE a member targets
//! rather than of the member, the operation or the request that carried the value:
//!
//! * a `@pattern` requirement quotes the expression the service PUBLISHES for the shape,
//!   which is the model's text without the whole-value anchors Smithy wraps it in
//!   (`::service_common::validation::unanchored`) — so a caller lifting the expression out
//!   of the message and compiling it locally gets the one the service advertises. The
//!   CHECK still runs the model's pattern as written; only the quoting drops the pair, and
//!   an expression the model leaves unanchored is quoted exactly as declared.
//! * a value outside an enumerated type is refused naming the values that type admits in
//!   the order the front door's own TABLE of them is walked — ascending bucket, the bucket
//!   being the low bits of the value's string hash
//!   ([`::service_common::validation_message::in_hashed_enum_order`], this tree's one home
//!   for that derivation) — and not in the order the model declares them in. The order is
//!   COMPUTED from the values themselves, so it belongs to the enumerated TYPE: every
//!   member targeting one enum is listed alike, in any operation and at any depth of the
//!   request, and a type the model gains needs nothing added here.
//!
//! Everything else stays the framework's: the counted envelope, the member paths, the
//! value each clause echoes, every other requirement's wording and the error code, so this
//! service cannot word a constraint differently from the services stating the same
//! sentence. A refusal that is not about the request's values — an unauthenticated caller
//! — is not this service's to word at all.

use ::service_common::validation_message::{aws_validation_refusal, in_hashed_enum_order};
use ::smithy_server_runtime::{Refusal, Refused};

/// How this service states `refusal`, or `None` to keep the framework's own wording.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    // Each violation is re-ordered BEFORE the envelope words it, so only the values of an
    // enum clause move and every other violation class reaches the envelope untouched.
    let stated = Refusal::Invalid(violations.iter().map(in_hashed_enum_order).collect());
    aws_validation_refusal(&stated)
}
