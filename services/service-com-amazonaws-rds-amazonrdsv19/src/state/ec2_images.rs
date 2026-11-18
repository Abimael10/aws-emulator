// SPDX-License-Identifier: CC0-1.0

//! The Amazon Machine Image a request names, as the service that OWNS images reports it.
//!
//! An AMI is not a resource of this service: it is Amazon EC2's. An account registers one
//! with `ec2:RegisterImage` (or creates it from an instance) and then names it in `ImageId`,
//! so EC2 is the ONE answer to "does this account have that image" — reached the way any
//! emulated service reaches another, through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! The question asked here is OWNERSHIP, not mere existence: an image is usable as the
//! basis of a custom engine version only if the calling account owns it, which is what
//! `DescribeImages`' `self` owner term selects. An image the account does not own is as
//! unusable as one that was never registered, and both are answered the same way.
//!
//! A hop that could not be made at all (no EC2 in this process, an answer that could not be
//! read) is evidence of nothing, so an image asked about that way is reported
//! [`ImageOwnership::Unknown`] rather than absent: a refusal has to be grounded in what EC2
//! said.
//!
//! Nothing is cached: an image registered a moment ago resolves at once, and one that was
//! deregistered stops resolving at once.

/// The `DescribeImages` filter that selects images BY ID (`image-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter is what asks the question rather than `ImageIds`: an identifier the account has
/// no image for is then an EMPTY answer instead of a failure of the whole call, which is
/// the answer this question needs — the caller is asking whether the account has the image,
/// not asking EC2 to assert that it does.
const IMAGE_ID_FILTER: &str = "image-id";

/// The `DescribeImages` owner term that selects the images the CALLING account owns
/// ("`self` — the sender of the request"), which is the ownership this question is about.
const OWNER_SELF: &str = "self";

/// What the owner of images answered about one identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOwnership {
    /// EC2 reports an image of that id owned by the calling account.
    OwnedByTheAccount,
    /// EC2 answered, and the account owns no image of that id.
    NotInTheAccount,
    /// Nothing is known: the hop could not be made, or the owner answered with an error.
    Unknown,
}

/// Whether the calling account owns an image with the identifier `image_id`, in `region`,
/// as EC2 reports it.
///
/// Asked with ONE hop carrying one `image-id` filter and the `self` owner term, so the
/// answer is about the account's own images and the cost of the question does not grow with
/// the request.
pub async fn image_ownership(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    image_id: &str,
) -> ImageOwnership {
    let Some(client) = super::ec2_subnets::ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return ImageOwnership::Unknown;
    };
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(IMAGE_ID_FILTER)
        .values(image_id)
        .build();
    match client
        .describe_images()
        .owners(OWNER_SELF)
        .filters(by_id)
        .send()
        .await
    {
        Ok(output) => {
            let held = output
                .images()
                .iter()
                .any(|image| image.image_id() == Some(image_id));
            if held {
                ImageOwnership::OwnedByTheAccount
            } else {
                ImageOwnership::NotInTheAccount
            }
        }
        Err(error) => {
            // At `warn` for the same reason the subnet and security-group hops log one: a
            // deployment where this hop fails is one where no image a request names can be
            // resolved, and it IS why nothing was refused.
            ::tracing::warn!(
                %region,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the AMI a request named could not be resolved against EC2"
            );
            ImageOwnership::Unknown
        }
    }
}
