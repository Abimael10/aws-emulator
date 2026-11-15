// SPDX-License-Identifier: CC0-1.0

//! The Amazon-managed images a read of this service reaches: the base images AWS itself
//! publishes, as opposed to the ones an account built.
//!
//! An Image Builder image ARN carries the account that OWNS the image, and the modelled
//! `ImageVersionArnOrBuildVersionArn` a read takes admits two kinds of owner in that field:
//! an account id, or `aws(?:-[a-z-]+)?`
//! (`smithy_query(query="operation",operation="GetImage")`). The second is Amazon's own:
//! `ListImages`' `owner` names that catalog `Amazon` ("Amazon managed"), and
//! `CreateImageRecipe`'s `parentImage` documents an "Image Builder image Amazon Resource
//! Name (ARN)" as one of the identifiers a recipe's base image is named by, which is how a
//! recipe builds from a published image.
//!
//! Those images are not built from anything this account holds — nothing files them, no
//! recipe produces them and no build of this emulator can — so they are not records of the
//! build store ([`super::ImageBuildVersionRecord`]) but a CATALOG: one published build per
//! image, keyed by the name its ARN carries. A reference into it follows the same rule every
//! versioned reference of this service follows, so a wildcard version ARN resolves to the
//! published build rather than to nothing, which is what lets a caller discover the dated
//! build a wildcard reaches.
//!
//! The catalog this emulator holds is the one written below, which is NOT the whole of
//! Amazon's: AWS publishes a new dated build of each managed image as its contents change,
//! and holds images for operating systems and architectures beyond these. That is why a
//! recipe's `parentImage` is still not resolved against this catalog
//! ([`super::parent_image_resolved`]): a managed ARN this catalog does not carry establishes
//! nothing about whether the image exists, so a create naming one is stored rather than
//! refused.

use crate::ops::types::{Image, ImageState};

use super::{
    normalized_name, parent_image_platform, reported_tags, version_admits, VersionedReference,
    ARN_SERVICE, FIRST_BUILD_VERSION, IMAGE_ARN_PREFIX, IMAGE_STATUS_AVAILABLE, IMAGE_TYPE_AMI,
};

/// The account field an Amazon-managed image's ARN carries, in place of an account id: the
/// owner the modelled read pattern admits beside one (`aws`), and the owner `ListImages`
/// names `Amazon`.
pub const MANAGED_IMAGE_OWNER: &str = "aws";

/// The semantic version every image in this catalog is published at.
///
/// A managed image's version is the DATE of its build — "you might choose a software version
/// pattern, such as 1.0.0, or a date, such as 2021.01.01" (the modelled `version` member of
/// `Image`) — and AWS publishes a new dated build as an image's contents change. What those
/// dates ARE is published by AWS and by nothing in this emulator: no request, no state and no
/// computation here can derive one, and a date read from one recorded response would be that
/// run's, not the rule. So this catalog publishes ONE build of each image, at the date it
/// records here, and says so rather than implying otherwise: a reference naming the date AWS
/// published a build at reaches nothing here (it is not a build this catalog holds), exactly
/// as a reference to any version this emulator never built does.
/// [`MANAGED_IMAGE_DATE_CREATED`] is this same date as an instant.
pub const MANAGED_IMAGE_VERSION: &str = "2026.1.1";

/// The instant the images in this catalog were published, as a read reports it: the date
/// [`MANAGED_IMAGE_VERSION`] carries, rendered the way this model's string `DateTime` members
/// carry one ([`super::iso8601`]).
const MANAGED_IMAGE_DATE_CREATED: &str = "2026-01-01T00:00:00Z";

/// The value of the modelled `ImageSource` enum an image AWS publishes carries
/// (`crate::ops::types::IMAGE_SOURCE_VALUES`): "the origin of the base image" of a managed
/// image is Amazon itself.
const IMAGE_SOURCE_AMAZON_MANAGED: &str = "AMAZON_MANAGED";

/// One Amazon-managed image: what a read of it reports beyond what every entry shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedImage {
    /// The image's name as a read reports it; its ARN carries the [`normalized_name`] of it,
    /// which is the key a reference reaches it by.
    pub name: &'static str,
    /// "The operating system version for instances that launch from this image. For example,
    /// Amazon Linux 2, Ubuntu 18, or Microsoft Windows Server 2019" (the modelled `osVersion`
    /// member) — which a published image is the one kind of image this service can report,
    /// since the catalog says which operating system it carries.
    pub os_version: &'static str,
}

/// The Amazon-managed images this emulator holds, from the names Image Builder publishes
/// them under (the AWS documentation's managed-image list, as a recipe's `parentImage`
/// names one: `arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/x.x.x`). The
/// platform of each is read from its name, through this crate's one reading of a base
/// image's platform ([`parent_image_platform`]).
pub const MANAGED_IMAGES: &[ManagedImage] = &[
    ManagedImage {
        name: "Amazon Linux 2 x86",
        os_version: "Amazon Linux 2",
    },
    ManagedImage {
        name: "Amazon Linux 2 arm64",
        os_version: "Amazon Linux 2",
    },
    ManagedImage {
        name: "Amazon Linux 2023 x86",
        os_version: "Amazon Linux 2023",
    },
    ManagedImage {
        name: "Amazon Linux 2023 arm64",
        os_version: "Amazon Linux 2023",
    },
    ManagedImage {
        name: "Windows Server 2019 English Full Base x86",
        os_version: "Microsoft Windows Server 2019",
    },
    ManagedImage {
        name: "Windows Server 2022 English Full Base x86",
        os_version: "Microsoft Windows Server 2022",
    },
    ManagedImage {
        name: "Ubuntu Server 20 LTS x86",
        os_version: "Ubuntu 20.04",
    },
    ManagedImage {
        name: "Ubuntu Server 22 LTS x86",
        os_version: "Ubuntu 22.04",
    },
];

impl ManagedImage {
    /// The ARN of this image's published build in `region`.
    ///
    /// An image AWS publishes is published in every Region, so the ARN carries the Region the
    /// reference that reached it named; its account field is the catalog's owner rather than
    /// this account ([`MANAGED_IMAGE_OWNER`]).
    #[must_use]
    pub fn arn(&self, partition: &str, region: &str) -> String {
        format!(
            "arn:{partition}:{ARN_SERVICE}:{region}:{MANAGED_IMAGE_OWNER}:{IMAGE_ARN_PREFIX}{}/\
             {MANAGED_IMAGE_VERSION}/{FIRST_BUILD_VERSION}",
            normalized_name(self.name)
        )
    }

    /// This catalog entry as the modelled `Image` a read of it reports, in the partition and
    /// Region the reference that reached it named.
    ///
    /// What is reported is what the catalog knows: the image's name, the operating system it
    /// carries, the platform its name states and the build AWS published. Every other member
    /// belongs to a build of an ACCOUNT's own image — the recipe it was built from, the
    /// infrastructure it was built on, the tests it ran, the tags it carries — and a published
    /// image was built from none of them, so none is reported: "imported images don't contain
    /// a recipe" (the modelled `Image` shape), and an image AWS publishes is no more a
    /// product of this account's recipes than an imported one.
    #[must_use]
    pub fn view(&self, partition: &str, region: &str) -> Image {
        Image {
            arn: Some(self.arn(partition, region)),
            // "Specifies whether this image produces an AMI or a container image": every
            // image in this catalog is a base image an EC2 instance launches from.
            r#type: Some(IMAGE_TYPE_AMI.to_string()),
            name: Some(self.name.to_string()),
            version: Some(format!(
                "{MANAGED_IMAGE_VERSION}/{FIRST_BUILD_VERSION}"
            )),
            platform: Some(parent_image_platform(self.name)),
            enhanced_image_metadata_enabled: None,
            os_version: Some(self.os_version.to_string()),
            // A published image is one a caller can build from now: it has reached the
            // terminal status of the build that produced it.
            state: Some(ImageState {
                status: Some(IMAGE_STATUS_AVAILABLE.to_string()),
                reason: None,
                failure_context: None,
            }),
            image_recipe: None,
            container_recipe: None,
            source_pipeline_name: None,
            source_pipeline_arn: None,
            infrastructure_configuration: None,
            distribution_configuration: None,
            image_tests_configuration: None,
            date_created: Some(MANAGED_IMAGE_DATE_CREATED.to_string()),
            output_resources: None,
            tags: Some(reported_tags(None)),
            build_type: None,
            image_source: Some(IMAGE_SOURCE_AMAZON_MANAGED.to_string()),
            scan_state: None,
            image_scanning_configuration: None,
            deprecation_time: None,
            lifecycle_execution_id: None,
            execution_role: None,
            workflows: None,
            logging_configuration: None,
        }
    }
}

/// The published image the ARN `reference` reaches in `request_region`, as the modelled
/// `Image` a read reports, or `None` when it reaches none.
///
/// The reference is read exactly as one to an account's own image is — through this crate's
/// one reading of a versioned reference ([`VersionedReference`]) — so the name it carries
/// selects the entry, a trailing `x` wildcards a version node, and a build version ARN names
/// the published build. A reference naming another Region reaches nothing, since a regional
/// endpoint serves only its own Region.
#[must_use]
pub fn managed_image_view(reference: &str, request_region: &str) -> Option<Image> {
    let owner = ::service_common::arn::parse_of(reference, ARN_SERVICE)?;
    // An account id in the owner field names an image an ACCOUNT built, which this catalog
    // holds none of; those are the records of the build store.
    if ::service_common::arn::is_account_id(owner.account) {
        return None;
    }
    let reference = VersionedReference::parse(reference, IMAGE_ARN_PREFIX)?;
    // The catalog published ONE build of each image, so a build version ARN reaches it only
    // by that build number, and a version ARN only when its nodes admit the version it was
    // published at.
    if reference.region != request_region
        || reference
            .build_version
            .is_some_and(|build| build != FIRST_BUILD_VERSION)
        || !version_admits(&reference.version, MANAGED_IMAGE_VERSION)
    {
        return None;
    }
    MANAGED_IMAGES
        .iter()
        .find(|image| normalized_name(image.name) == reference.name)
        .map(|image| image.view(owner.partition, request_region))
}
