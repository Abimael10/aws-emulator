// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a status a resource's CURRENT status does not admit.
//!
//! The classification is modelled — `ValidationException` (HTTP 400) with `reason`
//! `InvalidStatus`, the value `ValidationExceptionReason` declares for a well-formed
//! request naming a status the resource's state does not allow — but the WORDS are not, so
//! they live here once rather than as a sentence per operation: a refusal states the SET of
//! statuses the update would have taken, so a caller can read the admissible values out of
//! the message and pick a legal one.
//!
//! The set itself is not decided here. It is the transition rule of the resource being
//! written (for an act, `crate::state::ActStatus::admitted`), so the sentence always names
//! what the operation actually admits for the state the resource is in.

use crate::ops::types::ValidationException;

/// The value `ValidationExceptionReason` declares for a status the resource's current
/// status does not admit: the request is well formed, but the status it names is not one
/// the resource can take from where it is.
const INVALID_STATUS: &str = "InvalidStatus";

/// How this service states the set of statuses a resource's current status admits:
/// bracketed, the values separated by `", "`, in the order the caller hands them over.
///
/// Nothing of the request is echoed — not the rejected value, not the resource's
/// identifiers — so the sentence carries only the admissible set and reads the same for
/// every caller that finds a resource in the same state.
pub fn status_must_be_one_of(admitted: &[&str]) -> String {
    format!("Status must be one of [{}]", admitted.join(", "))
}

/// The modelled refusal for a status the resource's current status does not admit:
/// `ValidationException` with reason `InvalidStatus` over [`status_must_be_one_of`].
///
/// No `fieldList` entry: that list is what `FieldValidationFailed` carries to point at an
/// offending member, and here the member's value is admissible in itself — it is the
/// resource's state that does not admit it.
pub fn invalid_status(admitted: &[&str]) -> ValidationException {
    ValidationException {
        message: status_must_be_one_of(admitted),
        reason: INVALID_STATUS.to_string(),
        field_list: None,
    }
}
