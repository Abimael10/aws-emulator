// SPDX-License-Identifier: CC0-1.0

//! How the tagging route refuses a `resourceArn` of ANOTHER vendor.
//!
//! The resource-tagging layer in front of this service reads a parsed ARN's VENDOR field
//! before it looks for any resource: an ARN of another service can name no resource of THIS
//! one however the account changes, so it is refused as a bad request about the value it was
//! handed, with a sentence that names the vendor it expected and echoes no part of the ARN
//! (`service_common::validation_message::expected_vendor`).
//!
//! Only the sentence lives here. Which refusal of the tagging route's ARN member this is,
//! and the class and status it carries, are [`super::tagging_resource_arn`]'s — the one
//! place that decision is taken, for all three tagging operations at once — so this module
//! answers one question about one parsed ARN and nothing else.

/// The sentence the tagging layer answers for `arn`, when `arn`'s vendor field names a
/// service other than this one; `None` for an ARN of this vendor, whose refusal — if the
/// rest of the modelled shape rejects it — is the model's own.
///
/// Takes the ARN already parsed by `service_common::arn` — the one parse of a
/// caller-supplied ARN in this tree — so this cannot disagree with the handlers that
/// resolve the resource such an ARN names.
pub(super) fn vendor_sentence(arn: &::service_common::arn::Arn<'_>) -> Option<String> {
    (arn.vendor != crate::state::ARN_NAMESPACE).then(|| {
        ::service_common::validation_message::expected_vendor(crate::state::ARN_NAMESPACE)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the sentence is answered for a well-formed resource ARN of another vendor, and
    /// for an ARN of this vendor there is none.
    #[test]
    fn only_another_vendors_resource_arn_gets_the_sentence() {
        // (case, the ARN as a caller wrote it, whether this layer words its refusal)
        let rows: &[(&str, &str, bool)] = &[
            ("an Amazon S3 bucket's ARN", "arn:aws:s3:::some-bucket", true),
            (
                "an AWS IoT thing's ARN",
                "arn:aws:iot:us-west-2:000000000000:thing/Core",
                true,
            ),
            (
                "an ARN of another partition's other vendor",
                "arn:aws-cn:lambda:cn-north-1:000000000000:function:f",
                true,
            ),
            (
                "this vendor's component version",
                "arn:aws:greengrass:us-west-2:000000000000:components:c:versions:1.0.0",
                false,
            ),
            (
                "this vendor's ARN, outside the rest of the shape",
                "arn:aws:greengrass:us-west-2:000000000000:things:Core",
                false,
            ),
        ];
        for (case, value, is_another_vendor) in rows {
            let arn = ::service_common::arn::parse(value)
                .unwrap_or_else(|| panic!("case {case:?}: the row's value is a well-formed ARN"));
            let sentence = vendor_sentence(&arn);
            assert_eq!(
                sentence.as_deref(),
                is_another_vendor
                    .then_some("Invalid input resource arn: expected vendor greengrass"),
                "case {case:?}: expected the layer to {} this ARN's vendor, got {sentence:?}",
                if *is_another_vendor {
                    "refuse"
                } else {
                    "accept"
                }
            );
        }
    }
}
