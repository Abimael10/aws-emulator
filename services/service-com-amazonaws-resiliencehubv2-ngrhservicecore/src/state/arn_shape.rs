// SPDX-License-Identifier: CC0-1.0

//! Whether an ARN a request supplies IS an ARN of one of this service's resources, and the
//! refusal for one that is not.
//!
//! `Arn`'s modelled pattern is GENERIC — it fixes the six ARN fields and lets the resource
//! segment be any `[A-Za-z0-9/][A-Za-z0-9:_/+.-]{0,1023}` — so a value that satisfies it
//! may name another AWS service's resource, another kind of this service's resource, or no
//! resource kind at all. The service decides WHAT an ARN names before it asks WHETHER that
//! resource exists: an ARN whose service field is not this service's namespace, or whose
//! resource segment is not `<kind>/<identifier>` for the kind the member requires, is a
//! fault in the request itself, so it is refused with the modelled `ValidationException`
//! (HTTP 400) worded [`ARN_VALIDATION_MESSAGE`] rather than reported as a resource that is
//! not there. Only an ARN that DOES name a resource of that kind may fall through to the
//! `ResourceNotFoundException` (HTTP 404) path.
//!
//! Where the resource would live — partition, Region and account — is no part of that
//! verdict: those fields say where to look rather than which kind of resource is meant, so
//! an ARN naming this kind of resource in another Region, another account or another
//! partition is well-formed and simply resolves to nothing stored.

use crate::ops::types::ValidationException;

use super::{ARN_SERVICE_NAMESPACE, SYSTEM_RESOURCE};

/// How this service words the refusal of an ARN that does not name the kind of resource the
/// member it was sent for requires.
///
/// The WHOLE message is fixed text: it reports that the value the caller sent is not an ARN
/// this member accepts, and interpolates nothing from the request — the caller has to fix
/// the ARN, and which ARN it sent is in the request it is holding.
pub const ARN_VALIDATION_MESSAGE: &str = "ARN validation error";

/// The first field of an ARN.
const ARN_PREFIX: &str = "arn";

/// How many `:`-separated fields an ARN has: `arn:<partition>:<service>:<region>:<account>:<resource>`.
/// The resource segment may carry further `:` separators, so the split stops at this many.
const ARN_FIELDS: usize = 6;

/// Whether `arn` names a resource of THIS service of kind `resource` (the `system` of a
/// `system/<id>` resource segment, say).
///
/// The service field must be this service's ARN namespace, and the resource segment must be
/// that kind followed by `/` and a non-empty identifier. Partition, Region and account are
/// not inspected — see this module's documentation.
pub fn names_resource_kind(arn: &str, resource: &str) -> bool {
    let mut fields = arn.splitn(ARN_FIELDS, ':');
    let partition_and_service = (fields.next(), fields.next(), fields.next());
    if partition_and_service.0 != Some(ARN_PREFIX) || partition_and_service.2 != Some(ARN_SERVICE_NAMESPACE) {
        return false;
    }
    // The Region and the account are read past rather than checked.
    let (_region, _account) = (fields.next(), fields.next());
    fields.next().is_some_and(|segment| {
        segment
            .strip_prefix(resource)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|identifier| !identifier.is_empty())
    })
}

/// The refusal for an ARN that does not name the kind of resource the member requires.
///
/// `reason` and `fieldList` are left absent: the sentence already says the value is not an
/// ARN this member takes, and neither the model nor this service's own wording fixes a
/// per-field entry for it.
pub fn arn_validation_error() -> ValidationException {
    ValidationException {
        message: ARN_VALIDATION_MESSAGE.to_string(),
        reason: None,
        field_list: None,
    }
}

/// The refusal for a `systemArn` member that does not name a system of this service, or
/// `None` when it does and existence is the next question.
///
/// Every operation that resolves a `systemArn` against the store reaches the verdict through
/// here, so the ARN-shape refusal cannot differ between two of them.
pub fn system_arn_rejection(arn: &str) -> Option<ValidationException> {
    (!names_resource_kind(arn, SYSTEM_RESOURCE)).then(arn_validation_error)
}
