// SPDX-License-Identifier: CC0-1.0

//! How the tagging route refuses a `resourceArn` it will not act on.
//!
//! `TagResource`, `UntagResource` and `ListTagsForResource` are served by the
//! resource-tagging layer in front of this service (`/tags/{resourceArn}`, by method), and
//! that layer PARSES the ARN before it looks for any resource. Two things it decides from
//! the value alone, and refuses as a bad request about the value it was handed — with its
//! own error code (`service_common::tagging_refusal`, which states that class once for
//! every service that answers it) rather than as a constraint the serving model states:
//!
//! * a value it cannot parse as an ARN at all is refused with the layer's own syntax
//!   sentence (`service_common::tagging_refusal::TAG_BAD_REQUEST_MESSAGE`), which is fixed
//!   text and echoes no part of the value, since the layer never reaches the fields such a
//!   value does not have;
//! * an ARN it parsed whose VENDOR field names another service is refused with the layer's
//!   vendor sentence ([`super::tagging_vendor`]), because an ARN of another service can
//!   name no resource of THIS one however the account changes.
//!
//! Both carry the same class on the wire and the same status; only the sentence differs,
//! and what chooses between them is the parse itself.
//!
//! WHY here rather than in each handler: the three operations bind that member to the same
//! `GenericV2ARN`, whose `@pattern` the framework refuses before any handler runs, so the
//! decision is already taken at the boundary and only its WORDING is this service's. Taking
//! it here words all three alike — including the keyless untag, which validates the same
//! input through the same seam ([`super::untag_without_keys`]) — and leaves the modelled
//! constraint as the check, so which values are refused stays the model's.
//!
//! An ARN of THIS vendor that breaks another part of the shape keeps the framework's
//! constraint envelope: the recorded refusals this module words are the two that layer
//! decides from the value's own syntax and vendor field, and nothing is claimed about a
//! well-formed Greengrass ARN the rest of the pattern rejects.

use aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use smithy_server_runtime::{Refusal, Refused};

/// The dotted path the validating walk names the tagging route's ARN member with: the
/// lowerCamel spelling of `resourceArn`, the member all three of those operations bind to
/// the `/tags/{resourceArn}` URI label and the only member of that name in this model.
const RESOURCE_ARN_PATH: &str = "resourceArn";

/// The value the tagging route's `resourceArn` carried, when `violation` is the `@pattern`
/// refusal of that member.
///
/// Keyed on the `@pattern` violation alone, which is the parse that layer performs: an
/// absent member, or another bound broken at the same member, is a different refusal and
/// keeps the framework's wording. A violation that withholds its value is likewise not one
/// of these refusals, since the layer states both sentences about a value it read.
fn refused_resource_arn(violation: &Violation) -> Option<&str> {
    (violation.path == RESOURCE_ARN_PATH && matches!(violation.kind, ViolationKind::Pattern(_)))
        .then_some(violation.value.as_deref())
        .flatten()
}

/// How the tagging layer words `refusal`, or `None` when `refusal` is not that layer
/// refusing the value it was handed.
///
/// Which values are ARNs, and whose, is `service_common::arn`'s reading — the one parse of
/// a caller-supplied ARN in this tree — so this cannot disagree with the handlers that
/// resolve the resource such an ARN names.
///
/// The HTTP status stays the protocol's — a constraint refusal is a 400, which is the
/// status that layer's bad-request class carries
/// (`service_common::tagging_refusal::TAG_BAD_REQUEST_STATUS`) — so only the code and the
/// sentence are chosen here.
pub(super) fn resource_arn_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let value = violations.iter().find_map(refused_resource_arn)?;
    let message = match ::service_common::arn::parse(value) {
        None => ::service_common::tagging_refusal::TAG_BAD_REQUEST_MESSAGE.to_string(),
        // A well-formed ARN: only its vendor field is this layer's to refuse, and an ARN
        // of this vendor leaves the refusal to the model that stated the constraint.
        Some(arn) => super::tagging_vendor::vendor_sentence(&arn)?,
    };
    Some(Refused {
        code: ::service_common::tagging_refusal::TAG_BAD_REQUEST_ERROR_CODE.to_string(),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed sentence the layer answers a value it could not parse as an ARN.
    const BAD_SYNTAX: &str = "Invalid input resource arn: bad syntax";

    /// The sentence it answers an ARN whose vendor field names another service.
    const EXPECTED_VENDOR: &str = "Invalid input resource arn: expected vendor greengrass";

    /// The class both carry on the wire.
    const BAD_REQUEST: &str = "BadRequestException";

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

    /// RULE: the layer's syntax sentence is answered for a value that is no ARN at all, its
    /// vendor sentence for a well-formed ARN of another vendor, and neither for anything
    /// else — each under the layer's own class.
    #[test]
    fn the_layer_words_each_refusal_of_the_value_it_was_handed() {
        // (case, the member path, the value it carried, the sentence the layer answers)
        let rows: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "a bare token with no arn: prefix",
                RESOURCE_ARN_PATH,
                "clearly-not-an-arn",
                Some(BAD_SYNTAX),
            ),
            ("an empty value", RESOURCE_ARN_PATH, "", Some(BAD_SYNTAX)),
            (
                "an ARN missing its resource field",
                RESOURCE_ARN_PATH,
                "arn:aws:greengrass:us-west-2:000000000000",
                Some(BAD_SYNTAX),
            ),
            (
                "six fields whose first is not `arn`",
                RESOURCE_ARN_PATH,
                "urn:aws:greengrass:us-west-2:000000000000:components:c",
                Some(BAD_SYNTAX),
            ),
            (
                "an ARN with an empty vendor field",
                RESOURCE_ARN_PATH,
                "arn:aws::us-west-2:000000000000:components:c",
                Some(BAD_SYNTAX),
            ),
            (
                "an Amazon S3 bucket's ARN",
                RESOURCE_ARN_PATH,
                "arn:aws:s3:::some-bucket",
                Some(EXPECTED_VENDOR),
            ),
            (
                "an AWS IoT thing's ARN",
                RESOURCE_ARN_PATH,
                "arn:aws:iot:us-west-2:000000000000:thing/Core",
                Some(EXPECTED_VENDOR),
            ),
            (
                "an ARN of another partition's other vendor",
                RESOURCE_ARN_PATH,
                "arn:aws-cn:lambda:cn-north-1:000000000000:function:f",
                Some(EXPECTED_VENDOR),
            ),
            (
                "this vendor's ARN, outside the rest of the shape",
                RESOURCE_ARN_PATH,
                "arn:aws:greengrass:us-west-2:000000000000:things:Core",
                None,
            ),
            (
                "another member carrying another vendor's ARN",
                "arn",
                "arn:aws:s3:::some-bucket",
                None,
            ),
            (
                "another member carrying a value that is no ARN",
                "arn",
                "clearly-not-an-arn",
                None,
            ),
        ];
        for (case, path, value, sentence) in rows {
            let refusal = Refusal::Invalid(vec![pattern_violation(path, value)]);
            let answered = resource_arn_refusal(&refusal);
            assert_eq!(
                answered.as_ref().map(|refused| refused.message.as_str()),
                *sentence,
                "case {case:?}: expected the layer's sentence {sentence:?}, got {answered:?}"
            );
            if let Some(answered) = answered {
                assert_eq!(
                    answered.code.as_str(),
                    BAD_REQUEST,
                    "case {case:?}: expected the tagging layer's class, got {answered:?}"
                );
            }
        }
    }

    /// RULE: an absent `resourceArn` is not one of these refusals — the layer states them
    /// about a value it read, so a request carrying none keeps the framework's wording.
    #[test]
    fn an_absent_resource_arn_is_not_one_of_these_refusals() {
        let refusal = Refusal::Invalid(vec![Violation {
            path: RESOURCE_ARN_PATH.to_string(),
            kind: ViolationKind::MissingRequiredMember,
            value: None,
            sensitive: false,
        }]);
        let answered = resource_arn_refusal(&refusal);
        assert!(
            answered.is_none(),
            "case \"an absent resourceArn\": expected the framework's wording, got {answered:?}"
        );
    }
}
