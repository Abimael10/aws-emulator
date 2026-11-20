// SPDX-License-Identifier: CC0-1.0

//! Whether SageMaker can PULL the container image a registration references out of Amazon
//! ECR, and the sentence a reference it cannot pull is refused with.
//!
//! The form of an image path is settled elsewhere (`super::container_image`). What is settled
//! here is the question that comes after it, for the operation whose recordings show it being
//! asked: a model-package registration resolves the Amazon ECR registry each inference
//! container's `Image` addresses, and is refused when that registry is not one the pull
//! SageMaker makes for this request can reach.
//!
//! Amazon ECR is a per-account, per-Region registry: "The Amazon Elastic Container Registry
//! (Amazon ECR) path where inference code is stored"
//! (`smithy_query(query="doc",shape="ModelPackageContainerDefinition",member="Image")`)
//! addresses one registry, spelled `<account>.dkr.ecr.<region>.<dnsSuffix>/<repository>`, and
//! the pull SageMaker makes for a registration is made in the Region the request is made in.
//! So a reference into another REGION addresses a registry no pull of this request reaches, and
//! two kinds of registry are left in the request's own Region:
//!
//! * this account's own, the one this emulator serves, each Region of it holding the
//!   repositories pushed to it;
//! * a registry AWS ITSELF publishes SageMaker's containers from — the same member documents
//!   the image as either the caller's own code or "an algorithm provided by SageMaker", and
//!   those algorithm, framework and deep-learning images are published from AWS-owned accounts,
//!   one per Region per family, whose repositories grant SageMaker the pull on any account's
//!   behalf ([`super::aws_container_registry`]).
//!
//! Every OTHER Amazon ECR registry — a third account's, or an account AWS publishes from in a
//! DIFFERENT Region than the one the host names — is a registry nothing grants this request a
//! pull from and nothing here holds a repository in, which is exactly the pull the recorded
//! service refuses.
//!
//! What this does NOT decide:
//!
//! * WHETHER the question is asked at all: a registration that asked for its model validation
//!   to be skipped (`SkipModelValidation`) is admitted without it, which the handler decides;
//! * a path that addresses no Amazon ECR registry at all (another registry's host, or a bare
//!   repository name) — the form of such a path is the only question the registration's
//!   recordings show being asked of it (`super::container_image`), and the refusal here names
//!   a registry id a path carrying no Amazon ECR host does not have;
//! * which images that registry HOLDS: the refusal is worded about the registry and the
//!   repository, so it is decided at that granularity, and a repository of a registry this
//!   request can pull from is one a push can fill at any time — including after the package is
//!   registered.

use ::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state as ecr_store;

use super::image_digest::reference_into_this_registry;

/// An Amazon ECR reference SageMaker cannot pull for this request, by the two values the
/// refusal names it with: the registry the path addresses and the repository inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpullableImage {
    /// The account id of the Amazon ECR registry the path addresses, which is the registry
    /// id the refusal reports.
    pub registry_id: String,
    /// The repository the path names inside that registry.
    pub repository_name: String,
}

/// The FIRST container image path of `images` that addresses an Amazon ECR registry SageMaker
/// cannot pull from for this request, in the order the request carries them — and `None` when
/// every one of them is a pull this request can make (or addresses no Amazon ECR registry at
/// all, which this does not decide).
///
/// A registration carrying SEVERAL containers settles them in the order the request wrote
/// them, so the refusal names the same image whichever others the request also carries.
#[must_use]
pub fn first_unpullable_image<'image>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    images: impl IntoIterator<Item = &'image str>,
) -> Option<UnpullableImage> {
    let request_region = super::request_region(ctx);
    images
        .into_iter()
        .find_map(|image| unpullable_image(ctx, &request_region, image))
}

/// Why SageMaker cannot pull `image` for a request made in `request_region`, and `None` when
/// it can — or when the path addresses no Amazon ECR registry, which this states nothing
/// about.
///
/// The path is read by Amazon ECR's OWN reading of a pull reference
/// (`state::registry_pull::parse_registry_reference`), so a path this service resolves and a
/// path that registry serves are the same paths, after a path carrying the address the
/// registry announces to a client is brought into the AWS-hosted spelling
/// ([`reference_into_this_registry`], the same step the image digest is resolved through).
///
/// What the reference has to satisfy is the Region of the pull and then the registry it
/// addresses inside it: this account's own, or one AWS publishes its SageMaker containers from
/// in that Region ([`super::aws_publishes_from`]).
fn unpullable_image(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    image: &str,
) -> Option<UnpullableImage> {
    let addressed = reference_into_this_registry(ctx, image);
    let (registry_id, reference) =
        ecr_store::registry_pull::parse_registry_reference(&addressed)?;
    let pullable = reference.region == request_region
        && (registry_id == ecr_store::registry_id()
            || super::aws_publishes_from(&reference.region, &registry_id));
    (!pullable).then_some(UnpullableImage {
        registry_id,
        repository_name: reference.repository_name,
    })
}

/// How SageMaker words an Amazon ECR reference it cannot pull: the registry and the
/// repository the path addressed, and the sentence telling the caller what to check.
#[must_use]
pub fn image_pull_access_denied(unpullable: &UnpullableImage) -> String {
    let UnpullableImage {
        registry_id,
        repository_name,
    } = unpullable;
    format!(
        "Access denied for registry ID: {registry_id}, repository name: {repository_name}. \
         Please check if your ECR image exists and has proper pull permissions for SageMaker."
    )
}
