// SPDX-License-Identifier: CC0-1.0

//! The base image a recipe builds from: which identifiers name one, and whether the image
//! the identifier names exists.
//!
//! `CreateImageRecipe`'s `parentImage` is "the base image for customizations specified in
//! the image recipe", and the member's own documentation lists the identifiers it may
//! carry: an "AMI ID", an "Image Builder image Amazon Resource Name (ARN)", an "Amazon Web
//! Services Systems Manager (SSM) Parameter Store Parameter, prefixed by `ssm:`, followed
//! by the parameter name or ARN", or an "Amazon Web Services Marketplace product ID". Its
//! model says only that the value is a non-empty string of at most 1024 characters, so both
//! questions a create has to answer about it are answered here:
//!
//! * is the value one of those identifier FORMS at all
//!   ([`is_supported_parent_image`])? A value that is none of them names no base image, so
//!   it is a value of that parameter this service cannot accept
//!   ([`unsupported_parent_image_message`]) and nothing is stored;
//! * does the image it names EXIST ([`parent_image_resolved`])? "If you enter an AMI ID or
//!   an SSM parameter that contains the AMI ID, you must have access to the AMI. The AMI
//!   must also be in the Region where you're creating the recipe" — an AMI is Amazon EC2's
//!   resource, so that is resolved against EC2 in the Region the request was signed for,
//!   before the recipe is stored, and an id EC2 reports no image for is reported MISSING
//!   ([`super::missing_resource_message`] under [`IMAGE_RESOURCE_TYPE`]).
//!
//! EC2 is reached the way any emulated service reaches another: the AWS SDK driven over the
//! in-process operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service crate and nothing leaves the process. Nothing is cached: an image registered a
//! moment ago resolves at once, and one that was deregistered stops resolving at once.
//!
//! What grounds a refusal is what EC2 ANSWERED. The two outcomes are different claims:
//!
//! * EC2 reported that the id names no image of this account in this Region
//!   (`InvalidAMIID.*`) — a fact about the value `parentImage` carried, so the create is
//!   refused as a missing resource and nothing is stored;
//! * nothing answered the lookup at all — this emulator was assembled with no EC2 to ask,
//!   or the Region a request was signed for is no Region label to address one in — so
//!   nothing about the image is established and the create proceeds rather than refusing a
//!   value no answer placed.
//!
//! The other three forms name catalogs no service of this emulator resolves an identifier
//! against — a Systems Manager public parameter, a Marketplace product, and an Image Builder
//! MANAGED image (`…:aws:image/…`), of which this store holds only the entries its own
//! catalog records ([`super::MANAGED_IMAGES`]) rather than the whole of Amazon's — so
//! nothing about them is established either and a recipe naming one is stored, for the same
//! reason.

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

/// The member the base image a recipe builds from arrives in, as the API addresses it —
/// the parameter a refusal of such a value names.
pub const PARENT_IMAGE_MEMBER: &str = "parentImage";

/// The resource-type token a refusal names an Amazon Machine Image by: EC2's own `Image`
/// resource, which an AMI id identifies.
pub const IMAGE_RESOURCE_TYPE: &str = "Image";

/// The prefix of an AMI id, the identifier EC2 prints an image with.
const AMI_ID_PREFIX: &str = "ami-";

/// The prefix the `parentImage` documentation requires in front of a Systems Manager
/// parameter: "prefixed by `ssm:`, followed by the parameter name or ARN".
const SSM_PARAMETER_PREFIX: &str = "ssm:";

/// The prefix of an Amazon Web Services Marketplace product id.
const MARKETPLACE_PRODUCT_PREFIX: &str = "prod-";

/// The requirement clause a `parentImage` written as an ARN that is not an image ARN of this
/// service is refused with.
///
/// A value carrying the fields of an ARN is an identifier of a KIND this service can read; it
/// is the FORM of that identifier — whose resource it names, and how its name and version are
/// written — that a base image reference has to follow, so the clause states exactly that.
const UNSUPPORTED_IDENTIFIER_FORMAT_REQUIREMENT: &str =
    "The supplied image identifier is not in a supported format.";

/// The requirement clause a `parentImage` that is no identifier of any kind is refused with.
///
/// Nothing about the value says what it meant to be, so the clause states the kinds of
/// identifier a base image may be named by — the member's own documented list, in the order
/// this class states it — and echoes nothing of the value.
const UNSUPPORTED_PARENT_IMAGE_KIND_REQUIREMENT: &str =
    "The provided parent image must be either an EC2 AMI Id, SSM Parameter, Marketplace \
     Product Id, or a valid Image Builder Image Arn.";

/// The prefix of every EC2 error code that says a value names no image of the account:
/// `InvalidAMIID.NotFound` for a well-formed id no image carries, `InvalidAMIID.Malformed`
/// for a value EC2 cannot read as an image id at all. Both say the same thing about a base
/// image.
const AMI_ID_REFUSAL_PREFIX: &str = "InvalidAMIID.";

/// Whether `value` is one of the identifier forms `parentImage` documents.
///
/// One reading of that list, so a create and any later reader of a stored recipe agree on
/// which values name a base image:
///
/// * an AMI id — the `ami-` prefix EC2 prints an image id with, followed by the hexadecimal
///   identifier; how LONG that identifier is, is EC2's own question, answered where the id
///   is resolved ([`parent_image_resolved`]);
/// * an Image Builder image ARN, read as this service reads every reference to one of its
///   own image resources ([`super::VersionedReference`] under
///   [`super::IMAGE_ARN_PREFIX`]), so a managed image's wildcard version ARN
///   (`…:aws:image/<name>/x.x.x`) is one;
/// * a Systems Manager parameter, which the member requires the [`SSM_PARAMETER_PREFIX`] in
///   front of, followed by the parameter name or ARN;
/// * a Marketplace product id, which carries that catalog's own [`MARKETPLACE_PRODUCT_PREFIX`].
#[must_use]
pub fn is_supported_parent_image(value: &str) -> bool {
    is_ami_id(value)
        || super::VersionedReference::parse(value, super::IMAGE_ARN_PREFIX).is_some()
        || named_after(value, SSM_PARAMETER_PREFIX)
        || named_after(value, MARKETPLACE_PRODUCT_PREFIX)
}

/// Whether `value` is an AMI id: the `ami-` prefix followed by a hexadecimal identifier, as
/// EC2 prints one.
#[must_use]
pub fn is_ami_id(value: &str) -> bool {
    value
        .strip_prefix(AMI_ID_PREFIX)
        .is_some_and(|identifier| {
            !identifier.is_empty()
                && identifier
                    .chars()
                    .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
        })
}

/// Whether `value` carries `prefix` and names something after it.
fn named_after(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|named| !named.is_empty())
}

/// Why a create naming a base image in none of the documented forms is refused.
///
/// Stated in the sentence this service states every refused parameter in
/// (`service_common::validation_message::parameter_value_mismatch`), whose only variable
/// part is the member the value arrived in. Which requirement the sentence states depends on
/// what the value is as an identifier, which an ARN is the one self-describing form of
/// (`service_common::arn::has_arn_fields`, this tree's one reading of that question): a value
/// written as an ARN is an identifier whose FORM a base image reference does not admit
/// ([`UNSUPPORTED_IDENTIFIER_FORMAT_REQUIREMENT`]), while a value that is no identifier of any
/// kind is told which KINDS a base image may be named by
/// ([`UNSUPPORTED_PARENT_IMAGE_KIND_REQUIREMENT`]). Neither clause echoes the value.
#[must_use]
pub fn unsupported_parent_image_message(value: &str) -> String {
    let requirement = if ::service_common::arn::has_arn_fields(value) {
        UNSUPPORTED_IDENTIFIER_FORMAT_REQUIREMENT
    } else {
        UNSUPPORTED_PARENT_IMAGE_KIND_REQUIREMENT
    };
    ::service_common::validation_message::parameter_value_mismatch(PARENT_IMAGE_MEMBER, requirement)
}

/// Why a create naming a base image that does not exist is refused: the missing-resource
/// report every unresolvable reference of this service earns, naming the kind of resource
/// the request needed ([`IMAGE_RESOURCE_TYPE`]) and the identifier it named.
#[must_use]
pub fn missing_parent_image_message(reference: &str) -> String {
    super::missing_resource_message(IMAGE_RESOURCE_TYPE, reference)
}

/// What a lookup of the base image a request named established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentImageLookup {
    /// The image the identifier names exists where the recipe is being created.
    Held,
    /// The owning service answered that the identifier names no image there.
    Absent,
    /// Nothing established whether the image exists: no service answered the lookup, or the
    /// identifier names a catalog this emulator holds none of.
    Unestablished,
}

/// What the base image `reference` names, asked of the service that owns that identifier
/// form, in the Region this request was signed for.
///
/// An AMI id is Amazon EC2's, so EC2 is asked about that one. Every other documented form
/// names a catalog no service of this emulator holds, so nothing about it is established
/// (see the module docs).
pub async fn parent_image_resolved(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    reference: &str,
) -> ParentImageLookup {
    if !is_ami_id(reference) {
        return ParentImageLookup::Unestablished;
    }
    // No Region label, so no endpoint to ask EC2 at: the same answer an unreachable EC2
    // gives, which grounds no refusal.
    let Some(client) = ec2_client(ctx, region) else {
        return ParentImageLookup::Unestablished;
    };
    let answer = client
        .describe_images()
        .image_ids(reference)
        .send()
        .await;
    let error = match answer {
        // "Describes the specified images": EC2 reports one image per id it resolved, so an
        // answer carrying the image is the image existing, and one carrying none says the
        // same thing the refusal below does.
        Ok(output) => {
            return if output.images().is_empty() {
                ParentImageLookup::Absent
            } else {
                ParentImageLookup::Held
            }
        }
        Err(error) => error,
    };
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default();
    if code.starts_with(AMI_ID_REFUSAL_PREFIX) {
        return ParentImageLookup::Absent;
    }
    // At `warn`: a deployment where this hop fails is one where this resolution does not
    // run at all, which is worth seeing, and it IS why a base image was not resolved. The
    // caller's own value is rendered in its escaped `Debug` form, so an identifier carrying
    // a newline cannot forge a log line.
    ::tracing::warn!(
        parent_image = ?reference,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "a parent image could not be resolved against the emulator's EC2"
    );
    ParentImageLookup::Unestablished
}

/// The EC2 client this service asks about an AMI with, addressed to the Region the request
/// was signed for, or `None` when there is no Region label to ask in.
///
/// The ONE client of this crate for EC2's resources: the base image a create names is
/// resolved through it ([`parent_image_resolved`]) and so is the AMI an image build produces
/// ([`super::produce_output_ami`]), so both reach the same EC2 the same way.
///
/// The Region a request is signed for is CALLER-SUPPLIED text, and here it ends up in an
/// endpoint URL's authority rather than in a response, so it is read as a Region label
/// (`service_common::arn::region_label`, the tree's one reading of that grammar) instead of
/// being interpolated as given: a value carrying a host or path delimiter would name an
/// authority the caller chose rather than the peer this emulator asks.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a
/// stalled stream — and each of those would need a sleep implementation this client is not
/// given. The same goes for caching the identity: the credentials the callback hands over
/// are used directly.
pub fn ec2_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let label = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(label.to_string()))
        .endpoint_url(format!("https://ec2.{label}.amazonaws.com"))
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a value is a base image identifier exactly when it is one of the forms
    /// `parentImage` documents — an AMI id, an Image Builder image ARN, an `ssm:`-prefixed
    /// parameter, or a Marketplace product id — and nothing else is.
    #[test]
    fn the_documented_identifier_forms_are_the_ones_a_parent_image_may_carry() {
        // (case, the value, whether it names a base image)
        let rows: &[(&str, &str, bool)] = &[
            ("an AMI id", "ami-0123456789abcdef0", true),
            ("a short-form AMI id", "ami-1a2b3c4d", true),
            (
                "a managed image's wildcard version ARN",
                "arn:aws:imagebuilder:us-west-2:aws:image/amazon-linux-2-x86/x.x.x",
                true,
            ),
            (
                "an image build version ARN of this account",
                "arn:aws:imagebuilder:us-west-2:000000000000:image/my-image/1.0.0/1",
                true,
            ),
            (
                "an SSM parameter name",
                "ssm:/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64",
                true,
            ),
            ("a Marketplace product id", "prod-7y6s3l5i4rrgo", true),
            ("a value that names no identifier", "not-an-image-identifier", false),
            ("an AMI prefix with no identifier", "ami-", false),
            ("an AMI id carrying a non-hexadecimal node", "ami-zzzz", false),
            ("the SSM prefix with no parameter", "ssm:", false),
            ("an ARN of another vendor", "arn:aws:ec2:us-west-2:000000000000:image/ami-0123456789abcdef0", false),
            (
                "an image recipe ARN, which is no base image",
                "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/my-recipe/1.0.0",
                false,
            ),
            ("an empty value", "", false),
        ];
        for (case, value, expected) in rows {
            let actual = is_supported_parent_image(value);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected} for {value:?}, got {actual}"
            );
        }
    }

    /// RULE: the refusals state the member and the identifier the request carried, in the
    /// sentence each class is stated in — a value written as an ARN being told the form a base
    /// image identifier has to follow, and one that is no identifier at all being told which
    /// kinds of identifier a base image may be named by.
    #[test]
    fn the_parent_image_refusals_state_their_own_class() {
        let no_kind = unsupported_parent_image_message("not-an-image-identifier");
        assert_eq!(
            no_kind,
            "The value supplied for parameter 'parentImage' is not valid. The provided parent \
             image must be either an EC2 AMI Id, SSM Parameter, Marketplace Product Id, or a \
             valid Image Builder Image Arn.",
            "case \"a value that is no identifier\": got {no_kind:?}"
        );
        let unsupported_arn = unsupported_parent_image_message(
            "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/my-recipe/1.0.0",
        );
        assert_eq!(
            unsupported_arn,
            "The value supplied for parameter 'parentImage' is not valid. The supplied image \
             identifier is not in a supported format.",
            "case \"an ARN naming no base image\": got {unsupported_arn:?}"
        );
        let missing = missing_parent_image_message("ami-0123456789abcdef0");
        assert_eq!(
            missing,
            "The following required resource 'Image' cannot be found: 'ami-0123456789abcdef0'.",
            "case \"the missing-image refusal\": got {missing:?}"
        );
    }
}
