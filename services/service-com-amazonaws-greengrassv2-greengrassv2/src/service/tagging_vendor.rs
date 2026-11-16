// SPDX-License-Identifier: CC0-1.0

//! How the tagging route refuses a `resourceArn` of ANOTHER vendor.
//!
//! `TagResource`, `UntagResource` and `ListTagsForResource` are served by the
//! resource-tagging layer in front of this service (`/tags/{resourceArn}`, by method), and
//! that layer reads the ARN's VENDOR field before it looks for any resource: an ARN of
//! another service can name no resource of THIS one however the account changes, so it is
//! refused as a bad request about the value it was handed — with the layer's own error code
//! (`service_common::tagging_refusal`, which states that class once for every service that
//! answers it) and the layer's own sentence, which names the vendor it expected and echoes
//! no part of the ARN (`service_common::validation_message::expected_vendor`).
//!
//! WHY here rather than in each handler: the three operations bind that member to the same
//! `GenericV2ARN`, whose `@pattern` the framework refuses before any handler runs, so the
//! decision is already taken at the boundary and only its WORDING is this service's. Taking
//! it here words all three alike — including the keyless untag, which validates the same
//! input through the same seam ([`super::untag_without_keys`]) — and leaves the modelled
//! constraint as the check, so which values are refused stays the model's.
//!
//! Only the VENDOR case is reworded. A value that is no ARN at all, and an ARN of THIS
//! vendor that breaks another part of the shape, keep the framework's constraint envelope:
//! the recorded refusal this module states is the one AWS answers an ARN whose vendor field
//! names another service, and nothing is claimed about the others.

use aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use smithy_server_runtime::{Refusal, Refused};

/// The dotted path the validating walk names the tagging route's ARN member with: the
/// lowerCamel spelling of `resourceArn`, the member all three of those operations bind to
/// the `/tags/{resourceArn}` URI label and the only member of that name in this model.
const RESOURCE_ARN_PATH: &str = "resourceArn";

/// Whether `violation` is the tagging route refusing a `resourceArn` that is a well-formed
/// resource ARN of a vendor other than this service.
///
/// Which values are ARNs, and whose, is `service_common::arn`'s reading — the one parse of
/// a caller-supplied ARN in this tree — so this cannot disagree with the handlers that
/// resolve the resource such an ARN names. Keyed on the `@pattern` violation alone, which
/// is the parse that layer performs: an absent member, or another bound broken at the same
/// member, is a different refusal and keeps the framework's wording.
fn names_another_vendor(violation: &Violation) -> bool {
    violation.path == RESOURCE_ARN_PATH
        && matches!(violation.kind, ViolationKind::Pattern(_))
        && violation.value.as_deref().is_some_and(|value| {
            ::service_common::arn::vendor(value)
                .is_some_and(|vendor| vendor != crate::state::ARN_NAMESPACE)
        })
}

/// The refusal above, or `None` when this is not that refusal.
///
/// The HTTP status stays the protocol's — a constraint refusal is a 400, which is the
/// status that layer's bad-request class carries
/// (`service_common::tagging_refusal::TAG_BAD_REQUEST_STATUS`) — so only the code and the
/// sentence are chosen here.
pub(super) fn vendor_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    violations.iter().find(|violation| names_another_vendor(violation))?;
    Some(Refused {
        code: ::service_common::tagging_refusal::TAG_BAD_REQUEST_ERROR_CODE.to_string(),
        message: ::service_common::validation_message::expected_vendor(crate::state::ARN_NAMESPACE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `@pattern` violation at `path`, echoing `value`, as the boundary walk reports
    /// one.
    fn pattern_violation(path: &str, value: &str) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::Pattern(
                "^arn:[^:]*:greengrass:[^:]*:(aws|[0-9]+):(components|deployments|coreDevices):"
                    .to_string(),
            ),
            value: Some(value.to_string()),
            sensitive: false,
        }
    }

    /// RULE: the sentence is answered for a well-formed resource ARN of another vendor at
    /// the tagging route's ARN member, and for nothing else.
    #[test]
    fn only_another_vendors_resource_arn_is_reworded() {
        // (case, the member path, the value it carried, whether this refusal is the one)
        let rows: &[(&str, &str, &str, bool)] = &[
            (
                "an Amazon S3 bucket's ARN",
                RESOURCE_ARN_PATH,
                "arn:aws:s3:::some-bucket",
                true,
            ),
            (
                "an AWS IoT thing's ARN",
                RESOURCE_ARN_PATH,
                "arn:aws:iot:us-west-2:000000000000:thing/Core",
                true,
            ),
            (
                "an ARN of another partition's other vendor",
                RESOURCE_ARN_PATH,
                "arn:aws-cn:lambda:cn-north-1:000000000000:function:f",
                true,
            ),
            (
                "a value that is no ARN at all",
                RESOURCE_ARN_PATH,
                "not-an-arn",
                false,
            ),
            (
                "this vendor's ARN, outside the rest of the shape",
                RESOURCE_ARN_PATH,
                "arn:aws:greengrass:us-west-2:000000000000:things:Core",
                false,
            ),
            (
                "another member carrying another vendor's ARN",
                "arn",
                "arn:aws:s3:::some-bucket",
                false,
            ),
        ];
        for (case, path, value, is_the_refusal) in rows {
            let refusal = Refusal::Invalid(vec![pattern_violation(path, value)]);
            let answered = vendor_refusal(&refusal);
            assert_eq!(
                answered.is_some(),
                *is_the_refusal,
                "case {case:?}: expected the vendor refusal to {} it, got {answered:?}",
                if *is_the_refusal { "claim" } else { "decline" }
            );
            if let Some(answered) = answered {
                assert_eq!(
                    (answered.code.as_str(), answered.message.as_str()),
                    (
                        "BadRequestException",
                        "Invalid input resource arn: expected vendor greengrass"
                    ),
                    "case {case:?}: expected the tagging layer's class and sentence, got \
                     {answered:?}"
                );
            }
        }
    }

    /// RULE: an absent `resourceArn` is not this refusal — the layer states it about a
    /// value it read, so a request carrying none keeps the framework's wording.
    #[test]
    fn an_absent_resource_arn_is_not_this_refusal() {
        let refusal = Refusal::Invalid(vec![Violation {
            path: RESOURCE_ARN_PATH.to_string(),
            kind: ViolationKind::MissingRequiredMember,
            value: None,
            sensitive: false,
        }]);
        let answered = vendor_refusal(&refusal);
        assert!(
            answered.is_none(),
            "case \"an absent resourceArn\": expected the framework's wording, got {answered:?}"
        );
    }
}
