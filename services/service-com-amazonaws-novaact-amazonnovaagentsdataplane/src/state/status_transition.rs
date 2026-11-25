// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a status a resource's CURRENT status does not admit, and a
//! status write against a resource whose state takes none.
//!
//! The classification is modelled — `ValidationException` (HTTP 400) with `reason`
//! `InvalidStatus`, the value `ValidationExceptionReason` declares for a well-formed
//! request naming a status the resource's state does not allow — but the WORDS are not, so
//! they live here once rather than as a sentence per operation. There are two, each as this
//! service's recorded refusal words it. One states the SET of statuses the resource's
//! current status admits, so a caller can read the admissible values out of the message and
//! pick a legal one; the other names the KIND of resource that is in a state the write did
//! not expect, with no set in it at all. Which of the two a refusal carries is the handler's
//! to decide: a workflow run states only the second, while an act states the first for a
//! status it admits no caller to submit and the second for an act whose state takes no
//! further status change at all.
//!
//! WHICH statuses are admitted is not decided here. That is the transition rule of the
//! resource being written (`crate::state::ActStatus::admits` and `admitted` for an act,
//! `crate::state::WorkflowRunStatus::admits` for a run); this module only states the
//! refusal once a handler has read that rule, and the set-stating sentence always carries
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

/// How this service states that the resource a status write names is in a state that write
/// did not expect: the KIND of resource — named as the operations that work on it name it,
/// so a run is a `WorkflowRun` and an act an `Act` — then fixed text.
///
/// This is the wording for a refusal that states no admissible set — whatever the current
/// status does admit, the sentence is the same; where the refusal states the set instead,
/// [`status_must_be_one_of`] words it. Nothing of the request is echoed — not the rejected
/// status, not the resource's identifiers, not the status it holds — so the sentence reads
/// the same for every caller a resource of that kind refuses.
pub fn resource_in_unexpected_state(resource_kind: &str) -> String {
    format!("{resource_kind} in unexpected state")
}

/// The modelled refusal for a status write on a resource of `resource_kind` whose current
/// status does not admit the status named, or whose state takes no status change at all:
/// `ValidationException` with reason `InvalidStatus` over [`resource_in_unexpected_state`].
/// Such a status is a bad value for the member the request carries, not a conflict with the
/// resource, so this is the same classification [`invalid_status`] carries.
pub fn in_unexpected_state(resource_kind: &str) -> ValidationException {
    ValidationException {
        message: resource_in_unexpected_state(resource_kind),
        reason: INVALID_STATUS.to_string(),
        field_list: None,
    }
}
