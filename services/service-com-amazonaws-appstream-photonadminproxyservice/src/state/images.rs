// SPDX-License-Identifier: CC0-1.0

//! The image catalog this service's store holds, and the record form it is written in.
//!
//! An AppStream image is either one AWS publishes for every account to launch fleets and
//! image builders from — a base image — or a private one an account mints in its own
//! Region (an image builder's snapshot, an imported image, a copy, a managed update). No
//! account creates a base image, so the published ones are SEEDED into this service's
//! store ([`seed_image_catalog`]) and only read from, while every image an operation of
//! this API mints is written into the SAME map: `DescribeImages` reads one catalog, so a
//! private image and a published one are addressed, filtered and paged by exactly the
//! same rules.
//!
//! The shipped table below is a fixed snapshot of the base-image families AWS publishes,
//! written with AWS's own base-image naming
//! (`AppStream-[Graphics-<accelerator>-]<operating system>-<MM-DD-YYYY>`, the release date
//! of the published image). It carries, for every release batch the catalog publishes
//! ([`PUBLISHED_RELEASE_BATCHES`]) and every operating system this API models an image
//! platform for, the general-purpose base image, and for every operating system AWS
//! publishes accelerated images for, one base image per graphics accelerator — so a client
//! choosing an image finds one for each instance family it can launch a fleet on, under
//! every batch still published rather than under one frozen date. The platform tokens are
//! `PlatformType`'s (`PLATFORM_TYPE_VALUES` in
//! `ops/types/shared_types.rs`), and the supported instance families are the ones the
//! model's `Image$SupportedInstanceFamilies` doc enumerates
//! (`smithy_query(query="doc", shape="Image", member="SupportedInstanceFamilies")`:
//! General Purpose, Compute Optimized, Memory Optimized, Graphics G4, Graphics G5,
//! Graphics G6).
//!
//! What the catalog PUBLISHES about an image is carried in the table rather than rendered
//! from the row's name or accelerator: the description AWS words per operating system and
//! per accelerator, the recipe the image was built from (its name, its revision and the
//! build service that owns it) and the GPU driver the batch's accelerated images carry are
//! each published data, and no two of them can be derived from one another.
//!
//! A row carries only what the published image states: the members an account's own
//! activity fills — the image builder that produced it, the base image it was created
//! from, its applications, its sharing permissions, its errors — are ABSENT on a base
//! image, and a private image record fills them.

use std::collections::BTreeMap;

use crate::ops::types::{
    Application, Image, ImagePermissions, ImageSoftwareMetadata, ImageStateChangeReason,
    ImageVolumeConfig, ResourceError,
};

use super::{
    arn_resource, reported, reported_text, ComAmazonawsAppstreamPhotonadminproxyserviceState,
    ARN_NAMESPACE,
};

/// The ARN `resource-type` segment of an image, the segment before the image's name in
/// `arn:aws:appstream:<region>:<account>:image/<name>`.
pub const IMAGE_ARN_RESOURCE_TYPE: &str = "image";

/// The word AWS's refusal sentences use for an image, the `<kind>` slot of
/// [`super::resource_not_found_message`].
pub const IMAGE_KIND: &str = "image";

/// The `ImageState` an image that can be launched from carries
/// (`IMAGE_STATE_VALUES` in `ops/types/shared_types.rs`). The model's `Image$State` doc:
/// "The image starts in the `PENDING` state. If image creation succeeds, the state is
/// `AVAILABLE`." A published base image was created long before any request reaches this
/// emulator, so it is a terminal `AVAILABLE` the clock owes no transition out of.
pub const IMAGE_STATE_AVAILABLE: &str = "AVAILABLE";

/// The `VisibilityType` of an image AWS publishes to every account
/// (`VISIBILITY_TYPE_VALUES` in `ops/types/shared_types.rs`).
pub const VISIBILITY_PUBLIC: &str = "PUBLIC";

/// The `ImageType` of an AWS-published image. The model's `Image$ImageType` doc:
/// "Images created through AMI import have type \"custom\", while WorkSpaces Applications
/// provided images have type \"native\"" (`IMAGE_TYPE_VALUES` in
/// `ops/types/shared_types.rs`).
pub const IMAGE_TYPE_NATIVE: &str = "NATIVE";

/// The `DynamicAppProvidersEnabled` of an image that serves its applications from the
/// image itself (`DYNAMIC_APP_PROVIDERS_ENABLED_VALUES` in `ops/types/shared_types.rs`).
/// A published base image carries no dynamic app provider, so dynamic app providers are
/// not enabled within it.
pub const DYNAMIC_APP_PROVIDERS_DISABLED: &str = "DISABLED";

/// The `ImageSharedWithOthers` of an image that is shared with no other account
/// (`IMAGE_SHARED_WITH_OTHERS_VALUES` in `ops/types/shared_types.rs`). Only an account's
/// own private image can be shared, so a published base image is never shared by one.
pub const IMAGE_SHARED_WITH_OTHERS_FALSE: &str = "FALSE";

/// The `LatestAppstreamAgentVersion` of an image that does not carry the newest agent
/// (`LATEST_APPSTREAM_AGENT_VERSION_VALUES` in `ops/types/shared_types.rs`). A published
/// base image carries the agent of the batch it was released in rather than one that moves
/// as agents are released, so it reports that it is not on the latest.
pub const LATEST_APPSTREAM_AGENT_VERSION_FALSE: &str = "FALSE";

/// How many images one `DescribeImages` page carries.
///
/// The operation's `MaxResults` is `DescribeImagesMaxResults`, which the model bounds at
/// 25 (`smithy_query(query="operation", operation="DescribeImages")`: `range 0..25`), so
/// no page of this read is larger than that whatever a request asks for.
pub const DESCRIBE_IMAGES_PAGE_SIZE: usize = 25;

/// `arn:aws:appstream:<region>:<account>:image/<name>` — the ARN of an image an account
/// minted in `region`.
///
/// Nothing calls it yet: every operation that mints an image (a builder's snapshot, an
/// import, a copy, a managed update) is still a scaffold, so every image the store holds
/// today is a published one and takes [`published_image_arn`] instead. It states the owned
/// form for the create that fills `ImageRecord::arn`.
pub fn image_arn(region: &str, name: &str) -> String {
    ::emulator_server::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{IMAGE_ARN_RESOURCE_TYPE}/{name}"),
    )
}

/// `arn:aws:appstream:<region>::image/<name>` — the ARN of an image AWS publishes.
///
/// No account owns a published base image: AWS publishes the same image to every account,
/// so the ARN's account field is EMPTY, the way the ARN of any resource the service itself
/// owns names no account. An image an account minted in a Region carries that account
/// ([`image_arn`], the owned counterpart of this builder) — which is why the shared
/// `emulator_server::emulator_arn` helper cannot build this form: it always stamps the
/// emulator's account into the ARN it returns, so only the owned builder goes through it.
pub fn published_image_arn(region: &str, name: &str) -> String {
    format!("arn:aws:{ARN_NAMESPACE}:{region}::{IMAGE_ARN_RESOURCE_TYPE}/{name}")
}

/// The ARN `resource-type` segment of an image recipe, the segment before the recipe's
/// name in the ARN of the published build an image was made from.
pub const RECIPE_ARN_RESOURCE_TYPE: &str = "recipe";

/// The ARN `service` segment of a published image recipe.
///
/// A recipe is NOT an AppStream resource: it is the image-build recipe of the AWS service
/// that builds the published AMIs this API's base images are, so the provenance ARN an
/// image reports names that service rather than this one — an `appstream` ARN built from
/// the image's own name names no resource at all and tells a client the wrong owning
/// service.
pub const RECIPE_ARN_NAMESPACE: &str = "amifarm";

/// The account the published recipes belong to: AWS builds its base images in its own
/// account, so the recipe ARN names that account and not the one the request is signed for
/// (which owns nothing of the build).
pub const RECIPE_PUBLISHER_ACCOUNT: &str = "121014824124";

/// `arn:aws:amifarm:<region>:<publisher>:recipe/<name>/<version>` — the ARN of the recipe
/// revision a published image was built from.
///
/// The recipe is named per operating system and accelerator and is shared by every image
/// built from it, so the revision the image came out of is the ARN's trailing segment: two
/// images of one operating system from two release batches name the same recipe under two
/// versions. The Region is the one the request asks about, the way the published image's own
/// ARN is ([`published_image_arn`]).
pub fn published_recipe_arn(region: &str, name: &str, version: u32) -> String {
    format!(
        "arn:aws:{RECIPE_ARN_NAMESPACE}:{region}:{RECIPE_PUBLISHER_ACCOUNT}:\
         {RECIPE_ARN_RESOURCE_TYPE}/{name}/{version}"
    )
}

/// The instance family of an image written as the TOKEN the `InstanceFamily` member
/// carries, given the family's display name as `Image$SupportedInstanceFamilies` enumerates
/// it ("General Purpose", "Graphics G4"): the same words in upper case, joined by
/// underscores.
fn instance_family_token(family: &str) -> String {
    family.to_uppercase().replace(' ', "_")
}

/// The size of an image's root volume in GB. The model's `VolumeConfig$VolumeSizeInGb`
/// doc: "Valid range is 200-500 GB. The default is 200 GB, which is included in the hourly
/// instance rate", so an image nobody has grown the volume of reports the default.
const ROOT_VOLUME_SIZE_IN_GB: i32 = 200;

/// The volume type an image's root volume is provisioned as, the member of the volume
/// configuration the on-disk model does not declare (`ops/types/image_wire.rs`).
const ROOT_VOLUME_TYPE: &str = "gp2";

/// An RFC 3339 instant as the modelled timestamp; the epoch for text that is not one, so
/// a mis-written table row cannot panic a read.
fn instant(rfc3339: &str) -> ::aws_smithy_types::DateTime {
    ::aws_smithy_types::DateTime::from_str(
        rfc3339,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|_| ::aws_smithy_types::DateTime::from_secs(0))
}

/// A stored AppStream image: a published base image the catalog ships, or one an
/// operation of this API minted for this account.
#[derive(Debug, Clone)]
pub struct ImageRecord {
    pub name: String,
    /// The ARN of an image minted in one Region, and `None` for a published base image.
    ///
    /// AWS publishes the same base images in every Region, each addressed by that
    /// Region's own ARN, so a base image has no one stored ARN: it reports the ARN of the
    /// Region the request addresses ([`ImageRecord::arn`]). An image this account minted
    /// belongs to the Region it was created in, so it carries the ARN the create stamped.
    pub arn: Option<String>,
    pub base_image_arn: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// One of `IMAGE_STATE_VALUES`.
    pub state: String,
    /// One of `VISIBILITY_TYPE_VALUES`.
    pub visibility: String,
    pub image_builder_supported: bool,
    pub image_builder_name: Option<String>,
    /// One of `PLATFORM_TYPE_VALUES`.
    pub platform: String,
    pub state_change_reason: Option<ImageStateChangeReason>,
    /// The applications the image carries; empty for an image that advertises none.
    pub applications: Vec<Application>,
    pub created_time: ::aws_smithy_types::DateTime,
    pub public_base_image_released_date: Option<::aws_smithy_types::DateTime>,
    pub appstream_agent_version: Option<String>,
    pub image_permissions: Option<ImagePermissions>,
    /// The errors of the image; empty for one nothing has gone wrong with.
    pub image_errors: Vec<ResourceError>,
    /// One of `LATEST_APPSTREAM_AGENT_VERSION_VALUES`.
    pub latest_appstream_agent_version: Option<String>,
    /// The instance families a fleet or image builder may launch this image on; empty for
    /// an image that states none.
    pub supported_instance_families: Vec<String>,
    /// The instance type groups the image serves, which an image that serves none reports
    /// as an EMPTY list rather than by leaving the member out: the groups are a property
    /// every image states, not a configuration an image can be without, so a client that
    /// walks them sees a list on every image.
    pub supported_instance_type_groups: Vec<String>,
    /// One of `DYNAMIC_APP_PROVIDERS_ENABLED_VALUES`.
    pub dynamic_app_providers_enabled: Option<String>,
    /// One of `IMAGE_SHARED_WITH_OTHERS_VALUES`.
    pub image_shared_with_others: Option<String>,
    pub managed_software_included: Option<bool>,
    /// One of `IMAGE_TYPE_VALUES`.
    pub image_type: Option<String>,
    pub image_software_metadata: Option<ImageSoftwareMetadata>,
    /// The one instance family the image runs on, as the member of that name states it:
    /// the family of [`Self::supported_instance_families`] written as a token
    /// ([`instance_family_token`]).
    pub instance_family: Option<String>,
    /// Whether the account brings the licence of the operating system the image runs.
    pub byol: Option<bool>,
    /// The configuration of the image's root volume.
    pub root_volume_config: Option<ImageVolumeConfig>,
    /// Whether the image serves a dynamic application catalog.
    pub dynamic_application_catalog_enabled: Option<bool>,
    /// The name of the published recipe the image was built from, and `None` for an image
    /// built from no published recipe. A recipe is addressed by the ARN of the Region the
    /// request asks about, the way the published image itself is ([`ImageRecord::arn`]), so
    /// the record keeps the recipe's NAME and REVISION and the read builds the ARN
    /// ([`published_recipe_arn`]).
    pub appstream_recipe_name: Option<String>,
    /// The revision of that recipe the image was built from, the trailing segment of the
    /// recipe ARN; `None` for an image built from no published recipe.
    pub appstream_recipe_version: Option<u32>,
    /// When that recipe revision was released.
    pub appstream_recipe_released: Option<::aws_smithy_types::DateTime>,
}

impl ImageRecord {
    /// The ARN this image reports to a request addressing `region`: the one its create
    /// stamped, or — for a published base image, which every Region and every account
    /// carries — that Region's accountless published ARN.
    pub fn arn(&self, region: &str) -> String {
        self.arn
            .clone()
            .unwrap_or_else(|| published_image_arn(region, &self.name))
    }

    /// Render the stored record as the modelled `Image` every reader returns.
    pub fn to_image(&self, region: &str) -> Image {
        Image {
            name: self.name.clone(),
            arn: Some(self.arn(region)),
            base_image_arn: self.base_image_arn.clone(),
            display_name: reported_text(self.display_name.as_deref()),
            state: Some(self.state.clone()),
            visibility: Some(self.visibility.clone()),
            image_builder_supported: Some(self.image_builder_supported),
            image_builder_name: self.image_builder_name.clone(),
            platform: Some(self.platform.clone()),
            description: reported_text(self.description.as_deref()),
            state_change_reason: self.state_change_reason.clone(),
            // An image reports the applications it carries and the errors it has, both as
            // a list that is PRESENT and empty when there are none: unlike a stack's
            // cleared attributes, neither list is a configuration an image can be without
            // — an image with no application catalog and nothing wrong with it reports two
            // empty lists.
            applications: Some(self.applications.clone()),
            created_time: Some(self.created_time),
            public_base_image_released_date: self.public_base_image_released_date,
            appstream_agent_version: self.appstream_agent_version.clone(),
            image_permissions: self.image_permissions.clone(),
            image_errors: Some(self.image_errors.clone()),
            latest_appstream_agent_version: self.latest_appstream_agent_version.clone(),
            supported_instance_families: reported(&self.supported_instance_families),
            // PRESENT and empty when the image serves no instance type group, so code that
            // iterates the groups of every reported image never meets a missing member.
            supported_instance_type_groups: Some(self.supported_instance_type_groups.clone()),
            dynamic_app_providers_enabled: self.dynamic_app_providers_enabled.clone(),
            image_shared_with_others: self.image_shared_with_others.clone(),
            managed_software_included: self.managed_software_included,
            image_type: self.image_type.clone(),
            image_software_metadata: self.image_software_metadata.clone(),
            instance_family: self.instance_family.clone(),
            byol: self.byol,
            root_volume_config: self.root_volume_config.clone(),
            dynamic_application_catalog_enabled: self.dynamic_application_catalog_enabled,
            appstream_recipe_arn: match (
                self.appstream_recipe_name.as_deref(),
                self.appstream_recipe_version,
            ) {
                (Some(recipe), Some(version)) => {
                    Some(published_recipe_arn(region, recipe, version))
                }
                _ => None,
            },
            appstream_recipe_release_timestamp: self.appstream_recipe_released,
        }
    }
}

/// The instance families a general-purpose base image supports: the non-graphics families
/// the model's `Image$SupportedInstanceFamilies` doc enumerates.
const GENERAL_INSTANCE_FAMILIES: &[&str] =
    &["General Purpose", "Compute Optimized", "Memory Optimized"];

/// The accelerator a published graphics base image is built for.
#[derive(Debug, Clone, Copy)]
struct Accelerator {
    /// The token the published image's name carries after `Graphics-`.
    token: &'static str,
    /// The `InstanceFamily` TOKEN such an image reports: the family of the accelerated
    /// hardware it was built for, which is the image's own family whatever set of families
    /// a fleet may launch it on.
    instance_family: &'static str,
    /// The instance families a fleet or image builder may launch such an image on, as the
    /// model's `Image$SupportedInstanceFamilies` doc names them
    /// (`smithy_query(query="doc", shape="Image", member="SupportedInstanceFamilies")`).
    /// An image built for ONE accelerator states that one family; an image built with the
    /// vendor's whole driver runs on every accelerated family of that vendor, so it states
    /// all of them.
    families: &'static [&'static str],
    /// The token the published recipe's name carries after the operating system
    /// (`AppStream<operating system>Graphics<this>Recipe`).
    recipe_token: &'static str,
}

/// The accelerated instance families of the model's `Image$SupportedInstanceFamilies`
/// enumeration (`Graphics G4`, `Graphics G5`, `Graphics G6`), and the ones AWS has
/// published accelerated images for since that doc was written, which it names by the same
/// `Graphics <generation>` convention.
const ACCELERATED_INSTANCE_FAMILIES: &[&str] =
    &["Graphics G4", "Graphics G5", "Graphics G6", "Graphics G7"];

/// The graphics accelerators the published catalog carries base images for: one per
/// accelerated instance family above — so a client choosing an image finds one for each
/// accelerated family it can launch a fleet or an image builder on — plus the image built
/// with the GPU vendor's generic driver, which is published under the vendor's own token
/// and runs on every one of those families.
const GRAPHICS_ACCELERATORS: &[Accelerator] = &[
    Accelerator {
        token: "G4dn",
        instance_family: "GRAPHICS_G4",
        families: &["Graphics G4"],
        recipe_token: "G4",
    },
    Accelerator {
        token: "G5",
        instance_family: "GRAPHICS_G5",
        families: &["Graphics G5"],
        recipe_token: "G5",
    },
    Accelerator {
        token: "G6",
        instance_family: "GRAPHICS_G6",
        families: &["Graphics G6"],
        recipe_token: "G6",
    },
    Accelerator {
        token: "G7",
        instance_family: "GRAPHICS_G7",
        families: &["Graphics G7"],
        recipe_token: "G7",
    },
    Accelerator {
        token: "NV",
        instance_family: "GRAPHICS_NV",
        families: ACCELERATED_INSTANCE_FAMILIES,
        recipe_token: "NV",
    },
];

/// One operating system the published catalog carries base images for.
#[derive(Debug, Clone, Copy)]
struct PublishedOs {
    /// The operating-system token a published image's name carries, and the one its
    /// published recipe's name carries (`AppStream<this>NonGraphicsRecipe`).
    token: &'static str,
    /// The subject of the description AWS publishes for this operating system's
    /// general-purpose base image: the words the published sentence opens with, before the
    /// clause stating what the image launches.
    ///
    /// PUBLISHED TEXT, not a rendering of [`Self::token`]: AWS words each operating system
    /// its own way (one names its distribution and its release in brackets, another spells
    /// its name out), so the sentence cannot be generated from the platform and is carried
    /// per operating system instead.
    general_description_subject: &'static str,
    /// The same subject for an ACCELERATED base image of this operating system, which AWS
    /// words separately from the general-purpose one.
    graphics_description_subject: &'static str,
    /// The image platform, one of `PLATFORM_TYPE_VALUES`.
    platform: &'static str,
    /// Whether AWS publishes accelerated base images for this operating system: a
    /// graphics image is published per accelerator in [`GRAPHICS_ACCELERATORS`] for one
    /// that has them, and none for one that does not.
    graphics: bool,
}

/// The operating systems the published catalog covers: every one this API models an image
/// platform for and still publishes base images under
/// (`PLATFORM_TYPE_VALUES` in `ops/types/shared_types.rs`; `WINDOWS` and
/// `WINDOWS_SERVER_2016` are the platforms of images published before this snapshot and
/// carry no current base image).
const PUBLISHED_OPERATING_SYSTEMS: &[PublishedOs] = &[
    PublishedOs {
        token: "AmazonLinux2",
        general_description_subject: "AppStream amazon linux 2 image",
        graphics_description_subject: "AppStream amazon linux 2 image",
        platform: "AMAZON_LINUX2",
        graphics: true,
    },
    PublishedOs {
        token: "RHEL8",
        general_description_subject: "AppStream redhat linux (RHEL 8) image",
        graphics_description_subject: "AppStream redhat linux image (RHEL 8)",
        platform: "RHEL8",
        graphics: true,
    },
    PublishedOs {
        token: "RockyLinux8",
        general_description_subject: "AppStream Rocky Linux 8 image",
        graphics_description_subject: "AppStream Rocky Linux 8 image",
        platform: "ROCKY_LINUX8",
        graphics: true,
    },
    PublishedOs {
        token: "UbuntuPro2404",
        general_description_subject: "AppStream Ubuntu Pro 24.04 image",
        graphics_description_subject: "AppStream Ubuntu Pro 24.04 image",
        platform: "UBUNTU_PRO_2404",
        graphics: false,
    },
    PublishedOs {
        token: "WinServer2019",
        general_description_subject: "AppStream Windows Server 2019 image",
        graphics_description_subject: "AppStream Windows Server 2019 image",
        platform: "WINDOWS_SERVER_2019",
        graphics: true,
    },
    PublishedOs {
        token: "WinServer2022",
        general_description_subject: "AppStream Windows Server 2022 image",
        graphics_description_subject: "AppStream Windows Server 2022 image",
        platform: "WINDOWS_SERVER_2022",
        graphics: true,
    },
    PublishedOs {
        token: "WinServer2025",
        general_description_subject: "AppStream Windows Server 2025 image",
        graphics_description_subject: "AppStream Windows Server 2025 image",
        platform: "WINDOWS_SERVER_2025",
        graphics: true,
    },
];

/// One release batch of the published base images this catalog ships.
#[derive(Debug, Clone, Copy)]
struct PublishedBatch {
    /// The day the batch was released, `YYYY-MM-DD`.
    released: &'static str,
    /// The NVIDIA GRID driver release the batch's ACCELERATED images were built with, and
    /// `None` for a batch whose published images state no software metadata.
    ///
    /// PUBLISHED per batch: AWS builds a batch's accelerated images from one driver
    /// release, and the batches that publish the driver's version are the ones whose
    /// snapshot states it. The model's `ImageSoftwareMetadata$nvidiaGridDriverVersion`
    /// doc: "The version of the NVIDIA GRID driver installed on the image. This field is
    /// empty if no NVIDIA GRID driver is installed", and its `@pattern` is
    /// `^[0-9]+(\.[0-9]+){1,2}$` (`smithy_query(query="shape",
    /// shape="ImageSoftwareMetadata")`).
    nvidia_grid_driver: Option<&'static str>,
}

/// The published base-image batches this catalog ships, oldest first.
///
/// AWS refreshes its published base images in batches and names every image of a batch
/// after the day the batch was released, so the date is the `<MM-DD-YYYY>` tail of each
/// image's name and the `PublicBaseImageReleasedDate` it reports. The shipped snapshot
/// transcribes every batch the catalog is known to publish. The images of every entry are
/// published SIDE BY SIDE, because a batch does not withdraw the one before it: an account
/// can still address an image of an older batch, and that is why this is a list rather
/// than the single most recent date. A batch released later is another entry here and
/// needs no other change.
const PUBLISHED_RELEASE_BATCHES: &[PublishedBatch] = &[
    PublishedBatch {
        released: "2024-06-17",
        nvidia_grid_driver: None,
    },
    PublishedBatch {
        released: "2025-05-30",
        nvidia_grid_driver: None,
    },
    PublishedBatch {
        released: "2025-07-28",
        nvidia_grid_driver: None,
    },
    PublishedBatch {
        released: "2025-09-05",
        nvidia_grid_driver: None,
    },
    PublishedBatch {
        released: "2025-11-10",
        nvidia_grid_driver: None,
    },
    PublishedBatch {
        released: "2026-02-18",
        nvidia_grid_driver: None,
    },
    PublishedBatch {
        released: "2026-04-16",
        nvidia_grid_driver: None,
    },
    PublishedBatch {
        released: "2026-08-31",
        nvidia_grid_driver: Some("580.82.07"),
    },
    PublishedBatch {
        released: "2026-09-22",
        nvidia_grid_driver: Some("580.95.05"),
    },
];

/// The revision the recipe of a GENERAL-PURPOSE base image carried in the oldest batch of
/// this snapshot.
///
/// A recipe is revised for every batch it publishes an image in, so the revision in a
/// published image's recipe ARN advances by one per batch ([`PublishedImage::recipe`]).
/// AWS's recipes were revised before this snapshot's oldest batch, so the count does not
/// start at one: the snapshot carries the revision the catalog publishes for its first
/// batch, and the non-graphics and the graphics recipe lines are a revision apart.
const GENERAL_FIRST_RECIPE_REVISION: u32 = 6;

/// The same for the recipe of an ACCELERATED base image.
const GRAPHICS_FIRST_RECIPE_REVISION: u32 = 5;

/// One published base image: an operating system, the accelerator it is built for when it
/// is a graphics image, and the release batch it belongs to.
#[derive(Debug, Clone, Copy)]
struct PublishedImage {
    os: PublishedOs,
    accelerator: Option<Accelerator>,
    batch: PublishedBatch,
    /// How many batches of [`PUBLISHED_RELEASE_BATCHES`] precede this image's batch: the
    /// number of revisions this image's recipe has had since the snapshot's first batch.
    batch_index: u32,
}

/// The date slot of a published image's name: the release date written `MM-DD-YYYY`, the
/// form AWS's base-image names carry it in.
fn name_date(released: &str) -> String {
    let mut fields = released.split('-');
    match (fields.next(), fields.next(), fields.next()) {
        (Some(year), Some(month), Some(day)) => format!("{month}-{day}-{year}"),
        _ => released.to_string(),
    }
}

impl PublishedImage {
    /// The name a fleet or image builder names this image by:
    /// `AppStream-[Graphics-<accelerator>-]<operating system>-<MM-DD-YYYY>`, AWS's naming
    /// of its published base images.
    fn name(&self) -> String {
        let date = name_date(self.batch.released);
        let os = self.os.token;
        match self.accelerator {
            Some(accelerator) => {
                format!("AppStream-Graphics-{}-{os}-{date}", accelerator.token)
            }
            None => format!("AppStream-{os}-{date}"),
        }
    }

    /// The description AWS publishes for the image: the subject its operating system is
    /// published under, and the clause stating what the image launches — an accelerated
    /// image names the accelerator the instances it launches carry.
    ///
    /// The subject is the operating system's PUBLISHED text and not a rendering of its
    /// platform or its name token ([`PublishedOs::general_description_subject`]).
    fn description(&self) -> String {
        match self.accelerator {
            Some(accelerator) => format!(
                "{} for launching Graphics {} image builder instances",
                self.os.graphics_description_subject, accelerator.token
            ),
            None => format!(
                "{} for launching image builder instances",
                self.os.general_description_subject
            ),
        }
    }

    /// The instance families a fleet or image builder may launch this image on: the ones
    /// a graphics image's accelerator serves, or the general-purpose families.
    fn supported_instance_families(&self) -> Vec<String> {
        match self.accelerator {
            Some(accelerator) => accelerator
                .families
                .iter()
                .map(|family| (*family).to_string())
                .collect(),
            None => GENERAL_INSTANCE_FAMILIES
                .iter()
                .map(|family| (*family).to_string())
                .collect(),
        }
    }

    /// The `InstanceFamily` token the image reports: the accelerated family a graphics
    /// image's hardware is, and for a general-purpose image the first of the families it
    /// serves, written as a token ([`instance_family_token`]).
    fn instance_family(&self) -> Option<String> {
        match self.accelerator {
            Some(accelerator) => Some(accelerator.instance_family.to_string()),
            None => GENERAL_INSTANCE_FAMILIES
                .first()
                .map(|family| instance_family_token(family)),
        }
    }

    /// The published recipe this image was built from: its name — one recipe per operating
    /// system and accelerator, shared by every batch's image of that combination — and the
    /// revision of it this batch was built from.
    fn recipe(&self) -> (String, u32) {
        match self.accelerator {
            Some(accelerator) => (
                format!(
                    "AppStream{}Graphics{}Recipe",
                    self.os.token, accelerator.recipe_token
                ),
                GRAPHICS_FIRST_RECIPE_REVISION + self.batch_index,
            ),
            None => (
                format!("AppStream{}NonGraphicsRecipe", self.os.token),
                GENERAL_FIRST_RECIPE_REVISION + self.batch_index,
            ),
        }
    }

    /// The software metadata the image states: the NVIDIA GRID driver version its batch
    /// published for its accelerated images, and nothing for an image with no such driver
    /// installed — which is every general-purpose image, and every accelerated image of a
    /// batch that published no driver version.
    fn image_software_metadata(&self) -> Option<ImageSoftwareMetadata> {
        self.accelerator?;
        self.batch
            .nvidia_grid_driver
            .map(|version| ImageSoftwareMetadata {
                nvidia_grid_driver_version: Some(version.to_string()),
            })
    }

    /// The stored record this published image is read out of.
    fn record(&self) -> ImageRecord {
        let released = instant(&format!("{}T00:00:00Z", self.batch.released));
        let name = self.name();
        let (recipe_name, recipe_revision) = self.recipe();
        ImageRecord {
            // AWS publishes the image under its own name, so the name to display for it
            // is that name.
            display_name: Some(name.clone()),
            description: Some(self.description()),
            name,
            // Every Region and every account carries the published image, so its ARN is
            // the accountless one of the Region a request addresses rather than a stored
            // one.
            arn: None,
            base_image_arn: None,
            state: IMAGE_STATE_AVAILABLE.to_string(),
            visibility: VISIBILITY_PUBLIC.to_string(),
            // "Indicates whether an image builder can be launched from this image":
            // a published base image is what an image builder starts from.
            image_builder_supported: true,
            // No image builder produced it, and nothing has gone wrong with it.
            image_builder_name: None,
            platform: self.os.platform.to_string(),
            state_change_reason: None,
            applications: Vec::new(),
            // A published image was created when AWS released it.
            created_time: released,
            public_base_image_released_date: Some(released),
            appstream_agent_version: None,
            // Sharing permissions belong to an account's own private image.
            image_permissions: None,
            image_errors: Vec::new(),
            latest_appstream_agent_version: Some(
                LATEST_APPSTREAM_AGENT_VERSION_FALSE.to_string(),
            ),
            supported_instance_families: self.supported_instance_families(),
            // A base image AWS publishes serves no instance type group of its own, and
            // states so with an empty list rather than by leaving the member out.
            supported_instance_type_groups: Vec::new(),
            dynamic_app_providers_enabled: Some(DYNAMIC_APP_PROVIDERS_DISABLED.to_string()),
            image_shared_with_others: Some(IMAGE_SHARED_WITH_OTHERS_FALSE.to_string()),
            // A published base image includes no licence-included application.
            managed_software_included: Some(false),
            image_type: Some(IMAGE_TYPE_NATIVE.to_string()),
            image_software_metadata: self.image_software_metadata(),
            instance_family: self.instance_family(),
            // AWS publishes the licence of the operating system with the image, so the
            // account brings none of its own.
            byol: Some(false),
            root_volume_config: Some(ImageVolumeConfig {
                volume_size_in_gb: Some(ROOT_VOLUME_SIZE_IN_GB),
                volume_type: Some(ROOT_VOLUME_TYPE.to_string()),
            }),
            // The image carries the applications it was built with rather than a catalog
            // resolved at session start.
            dynamic_application_catalog_enabled: Some(false),
            // The recipe a base image was built from is the published build it came out
            // of: one recipe per operating system and accelerator, at the revision this
            // batch was built from.
            appstream_recipe_name: Some(recipe_name),
            appstream_recipe_version: Some(recipe_revision),
            appstream_recipe_released: Some(released),
        }
    }
}

/// Every base image AWS publishes to every account in the shipped snapshot: for each
/// release batch of [`PUBLISHED_RELEASE_BATCHES`], the general-purpose image of each
/// operating system of [`PUBLISHED_OPERATING_SYSTEMS`], and one graphics image per
/// accelerator of [`GRAPHICS_ACCELERATORS`] for each operating system that has accelerated
/// images — so a client choosing an image finds one for every platform and every instance
/// family it can launch a fleet or an image builder with, under every release batch still
/// published.
fn published_images() -> Vec<PublishedImage> {
    PUBLISHED_RELEASE_BATCHES
        .iter()
        .enumerate()
        .flat_map(|(index, batch)| {
            let batch_index = u32::try_from(index).unwrap_or_default();
            PUBLISHED_OPERATING_SYSTEMS.iter().flat_map(move |os| {
                let general = PublishedImage {
                    os: *os,
                    accelerator: None,
                    batch: *batch,
                    batch_index,
                };
                let graphics = GRAPHICS_ACCELERATORS
                    .iter()
                    .filter(|_| os.graphics)
                    .map(move |accelerator| PublishedImage {
                        os: *os,
                        accelerator: Some(*accelerator),
                        batch: *batch,
                        batch_index,
                    });
                ::std::iter::once(general).chain(graphics)
            })
        })
        .collect()
}

/// The image catalog a fresh store starts from: one record per published base image,
/// keyed by the name a fleet or image builder names it by.
///
/// BOUNDED by the shipped table: this seed adds no record per request, and an operation
/// that mints an image checks the map against
/// [`super::MAX_RECORDS_PER_STORE`] before inserting.
pub fn seed_image_catalog() -> BTreeMap<String, ImageRecord> {
    published_images()
        .iter()
        .map(|published| {
            let record = published.record();
            (record.name.clone(), record)
        })
        .collect()
}

impl ComAmazonawsAppstreamPhotonadminproxyserviceState {
    /// The image stored under the ARN asked about, as the Region `region` addresses it;
    /// `None` when this service holds none under it.
    ///
    /// An image is addressed by name or by ARN, and a stored ARN must match in full, so an
    /// ARN naming another Region's or another account's resource addresses no image here.
    /// The name in the ARN is what the record is keyed by, so the lookup is one map probe
    /// rather than a scan.
    pub fn image_by_arn(&self, region: &str, arn: &str) -> Option<&ImageRecord> {
        let (resource_type, name) = arn_resource(arn);
        (resource_type == IMAGE_ARN_RESOURCE_TYPE)
            .then(|| self.images.get(&name))
            .flatten()
            .filter(|record| record.arn(region) == arn)
    }
}
