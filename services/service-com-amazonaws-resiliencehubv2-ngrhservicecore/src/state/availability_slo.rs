// SPDX-License-Identifier: CC0-1.0

//! Which availability SLO target a resilience policy may carry.
//!
//! `AvailabilitySlo$target` is "the target availability percentage, expressed as a value
//! between 0 and 100" (`smithy_query(query="shape", shape="AvailabilitySlo")`), and the
//! model bounds it with `@range min 0 max 100` alone. The service itself offers only a
//! DISCRETE set of availability targets ([`ALLOWED_AVAILABILITY_SLO_TARGETS`]): a policy
//! can be measured against one of the service levels it publishes and against nothing
//! else, so a target the set does not hold is refused — with
//! [`unsupported_availability_slo_target_message`] — before anything is written, rather
//! than stored as an availability level no assessment could ever grade.
//!
//! The set lives HERE, in one place, so every operation that writes a policy's
//! availability SLO (`CreatePolicy`, `UpdatePolicy`, and `ImportPolicy` once it is
//! implemented) admits exactly the same targets and refuses the rest in the same words.
//!
//! What the model's own bounds refuse is NOT this module's: a target outside `0..=100`,
//! and a body whose `target` is not a number at all, break a rule the request schema
//! declares and are refused by the validating walk before any handler runs (see
//! `crate::service::schema_findings`). This refusal is about a value that satisfies every
//! modelled rule and still names no service level.

use crate::ops::types::{AvailabilitySlo, ValidationException};

/// The availability targets this service offers, as percentages.
///
/// The model enumerates none — it bounds the member with `@range min 0 max 100` only — so
/// the set is the service's own: the discrete availability levels it will measure a policy
/// against. The message a refusal carries lists exactly these values, so adding a level
/// here is all it takes for both the check and its wording to follow.
pub const ALLOWED_AVAILABILITY_SLO_TARGETS: [f64; 3] = [99.9, 99.95, 99.99];

/// The path of the member a refusal names, as the request spells it: the `availabilitySlo`
/// block's `target`. Every operation that takes the block binds it at this member name
/// (`smithy_query(query="operation", operation="CreatePolicy,UpdatePolicy")`).
pub const AVAILABILITY_SLO_TARGET_FIELD: &str = "availabilitySlo.target";

/// `ValidationExceptionReason`'s value for a member whose value is not one this service
/// accepts — the value is present and well formed, and what it names is not on offer.
const INVALID_FIELD_VALUE: &str = "INVALID_FIELD_VALUE";

/// The sentence a request carrying an availability target this service does not offer is
/// refused with.
///
/// Fixed text around the two things a caller can act on: the FIELD the offending value
/// arrived in, named by its request path, and the targets that are on offer, listed from
/// [`ALLOWED_AVAILABILITY_SLO_TARGETS`] and separated by `", "`. The value the request sent
/// is not part of it.
pub fn unsupported_availability_slo_target_message() -> String {
    let allowed = ALLOWED_AVAILABILITY_SLO_TARGETS
        .iter()
        .map(f64::to_string)
        .collect::<Vec<String>>()
        .join(", ");
    format!("Invalid input for field {AVAILABILITY_SLO_TARGET_FIELD}. Allowed values are [{allowed}].")
}

/// The refusal a request setting `availability_slo` is answered with, or `None` when the
/// block asks for nothing this service cannot offer.
///
/// `Some(..)` — the modelled `ValidationException` (HTTP 400), declared by every operation
/// that takes the block — exactly when the block states a `target`
/// [`ALLOWED_AVAILABILITY_SLO_TARGETS`] does not hold. `None` for a request that omits the
/// block, and for one that sends it without a `target`: neither states an availability
/// level, so there is no level to refuse.
pub fn availability_slo_rejection(
    availability_slo: Option<&AvailabilitySlo>,
) -> Option<ValidationException> {
    let target = availability_slo?.target?;
    if ALLOWED_AVAILABILITY_SLO_TARGETS.contains(&target) {
        return None;
    }
    Some(ValidationException {
        message: unsupported_availability_slo_target_message(),
        reason: Some(INVALID_FIELD_VALUE.to_string()),
        // `fieldList` entries carry a per-field sentence, and nothing in the model fixes
        // what that sentence is for this refusal, so the message carries it alone.
        field_list: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a target the service offers passes, every other value in the member's
    /// modelled range is refused, and a block that states no target states no level to
    /// refuse.
    #[test]
    fn only_a_target_the_service_offers_passes() {
        // (case, the block the request sent, whether it is refused)
        let rows: [(&str, Option<AvailabilitySlo>, bool); 9] = [
            ("no block at all", None, false),
            (
                "a block with no target",
                Some(AvailabilitySlo { target: None }),
                false,
            ),
            (
                "the lowest offered target",
                Some(AvailabilitySlo { target: Some(99.9) }),
                false,
            ),
            (
                "the middle offered target",
                Some(AvailabilitySlo {
                    target: Some(99.95),
                }),
                false,
            ),
            (
                "the highest offered target",
                Some(AvailabilitySlo {
                    target: Some(99.99),
                }),
                false,
            ),
            (
                "the member's modelled minimum",
                Some(AvailabilitySlo { target: Some(0.0) }),
                true,
            ),
            (
                "the member's modelled maximum",
                Some(AvailabilitySlo {
                    target: Some(100.0),
                }),
                true,
            ),
            (
                "a value just under an offered target",
                Some(AvailabilitySlo {
                    target: Some(99.89),
                }),
                true,
            ),
            (
                "a value between two offered targets",
                Some(AvailabilitySlo {
                    target: Some(99.97),
                }),
                true,
            ),
        ];
        for (case, block, refused) in rows {
            let rejection = availability_slo_rejection(block.as_ref());
            assert_eq!(
                rejection.is_some(),
                refused,
                "case {case:?}: expected refused={refused}, got {rejection:?}"
            );
        }
    }

    /// RULE: the refusal lists the targets the service offers, and names the member the
    /// value arrived in.
    #[test]
    fn the_refusal_lists_the_offered_targets() {
        let message = unsupported_availability_slo_target_message();
        assert_eq!(
            message, "Invalid input for field availabilitySlo.target. Allowed values are [99.9, 99.95, 99.99].",
            "unexpected wording {message:?}"
        );
    }
}
