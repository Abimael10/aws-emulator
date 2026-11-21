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
//! of the published image). It carries, for every operating system this API models an
//! image platform for, the general-purpose base image, and for every operating system AWS
//! publishes accelerated images for, one base image per graphics accelerator — so a client
//! choosing an image finds one for each instance family it can launch a fleet on. The
//! platform tokens are `PlatformType`'s (`PLATFORM_TYPE_VALUES` in
//! `ops/types/shared_types.rs`), and the supported instance families are the ones the
//! model's `Image$SupportedInstanceFamilies` doc enumerates
//! (`smithy_query(query="doc", shape="Image", member="SupportedInstanceFamilies")`:
//! General Purpose, Compute Optimized, Memory Optimized, Graphics G4, Graphics G5,
//! Graphics G6).
//!
//! A row carries only what the published image states: the members an account's own
//! activity fills — the image builder that produced it, the base image it was created
//! from, its applications, its sharing permissions, its errors — are ABSENT on a base
//! image, and a private image record fills them.

use std::collections::BTreeMap;

use crate::ops::types::{
    Application, Image, ImagePermissions, ImageSoftwareMetadata, ImageStateChangeReason,
    ResourceError,
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
    /// One of `DYNAMIC_APP_PROVIDERS_ENABLED_VALUES`.
    pub dynamic_app_providers_enabled: Option<String>,
    /// One of `IMAGE_SHARED_WITH_OTHERS_VALUES`.
    pub image_shared_with_others: Option<String>,
    pub managed_software_included: Option<bool>,
    /// One of `IMAGE_TYPE_VALUES`.
    pub image_type: Option<String>,
    pub image_software_metadata: Option<ImageSoftwareMetadata>,
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
            // A collection with no entries is something the image does not have, so the
            // member is ABSENT rather than present-and-empty — the same representation
            // every other resource of this service renders its collections with.
            applications: reported(&self.applications),
            created_time: Some(self.created_time),
            public_base_image_released_date: self.public_base_image_released_date,
            appstream_agent_version: self.appstream_agent_version.clone(),
            image_permissions: self.image_permissions.clone(),
            image_errors: reported(&self.image_errors),
            latest_appstream_agent_version: self.latest_appstream_agent_version.clone(),
            supported_instance_families: reported(&self.supported_instance_families),
            dynamic_app_providers_enabled: self.dynamic_app_providers_enabled.clone(),
            image_shared_with_others: self.image_shared_with_others.clone(),
            managed_software_included: self.managed_software_included,
            image_type: self.image_type.clone(),
            image_software_metadata: self.image_software_metadata.clone(),
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
    /// The instance family a fleet or image builder launches such an image on, as the
    /// model's `Image$SupportedInstanceFamilies` doc names it. A graphics image runs on
    /// the accelerated hardware it was built for and on no other, so this is the ONE
    /// family it states support for.
    family: &'static str,
}

/// One graphics accelerator per accelerated instance family the model's
/// `Image$SupportedInstanceFamilies` doc enumerates, so the catalog carries an image a
/// client can launch each of those families with.
const GRAPHICS_ACCELERATORS: &[Accelerator] = &[
    Accelerator {
        token: "G4dn",
        family: "Graphics G4",
    },
    Accelerator {
        token: "G5",
        family: "Graphics G5",
    },
    Accelerator {
        token: "G6",
        family: "Graphics G6",
    },
];

/// One operating system the published catalog carries base images for.
#[derive(Debug, Clone, Copy)]
struct PublishedOs {
    /// The operating-system token a published image's name carries.
    token: &'static str,
    /// What that operating system is called, the subject of the image's description.
    name: &'static str,
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
        name: "Amazon Linux 2",
        platform: "AMAZON_LINUX2",
        graphics: true,
    },
    PublishedOs {
        token: "RHEL8",
        name: "Red Hat Enterprise Linux 8",
        platform: "RHEL8",
        graphics: true,
    },
    PublishedOs {
        token: "RockyLinux8",
        name: "Rocky Linux 8",
        platform: "ROCKY_LINUX8",
        graphics: false,
    },
    PublishedOs {
        token: "UbuntuPro2404",
        name: "Ubuntu Pro 24.04",
        platform: "UBUNTU_PRO_2404",
        graphics: false,
    },
    PublishedOs {
        token: "WinServer2019",
        name: "Windows Server 2019",
        platform: "WINDOWS_SERVER_2019",
        graphics: true,
    },
    PublishedOs {
        token: "WinServer2022",
        name: "Windows Server 2022",
        platform: "WINDOWS_SERVER_2022",
        graphics: true,
    },
    PublishedOs {
        token: "WinServer2025",
        name: "Windows Server 2025",
        platform: "WINDOWS_SERVER_2025",
        graphics: true,
    },
];

/// The release dates of the published base-image batches this catalog ships,
/// `YYYY-MM-DD`.
///
/// AWS refreshes its published base images in batches and names every image of a batch
/// after the day the batch was released, so the date is the `<MM-DD-YYYY>` tail of each
/// image's name and the `PublicBaseImageReleasedDate` it reports. The shipped snapshot
/// transcribes one such batch — the one this crate's catalog has carried since it was
/// seeded, the `06-17-2024` tail of the published names it was written from; a batch AWS
/// releases later is another entry here, and the images of every entry are published side
/// by side, because a batch does not withdraw the one before it — which is why this is a
/// list of batches rather than a single date, even while the snapshot ships one.
const PUBLISHED_RELEASE_DATES: &[&str] = &["2024-06-17"];

/// One published base image: an operating system, the accelerator it is built for when it
/// is a graphics image, and the release date of the batch it belongs to.
#[derive(Debug, Clone, Copy)]
struct PublishedImage {
    os: PublishedOs,
    accelerator: Option<Accelerator>,
    released: &'static str,
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
        let date = name_date(self.released);
        let os = self.os.token;
        match self.accelerator {
            Some(accelerator) => {
                format!("AppStream-Graphics-{}-{os}-{date}", accelerator.token)
            }
            None => format!("AppStream-{os}-{date}"),
        }
    }

    /// What the image is: the operating system it runs, and the accelerator it is built
    /// for when it is a graphics image.
    fn description(&self) -> String {
        let os = self.os.name;
        match self.accelerator {
            Some(accelerator) => format!(
                "AppStream 2.0 {} graphics base image for {os}",
                accelerator.token
            ),
            None => format!("AppStream 2.0 base image for {os}"),
        }
    }

    /// The instance families a fleet or image builder may launch this image on: the one
    /// family a graphics image's accelerator serves, or the general-purpose families.
    fn supported_instance_families(&self) -> Vec<String> {
        match self.accelerator {
            Some(accelerator) => vec![accelerator.family.to_string()],
            None => GENERAL_INSTANCE_FAMILIES
                .iter()
                .map(|family| (*family).to_string())
                .collect(),
        }
    }

    /// The stored record this published image is read out of.
    fn record(&self) -> ImageRecord {
        let released = instant(&format!("{}T00:00:00Z", self.released));
        let name = self.name();
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
            latest_appstream_agent_version: None,
            supported_instance_families: self.supported_instance_families(),
            dynamic_app_providers_enabled: None,
            image_shared_with_others: None,
            managed_software_included: None,
            image_type: Some(IMAGE_TYPE_NATIVE.to_string()),
            image_software_metadata: None,
        }
    }
}

/// Every base image AWS publishes to every account in the shipped snapshot: for each
/// release date of [`PUBLISHED_RELEASE_DATES`], the general-purpose image of each operating
/// system of [`PUBLISHED_OPERATING_SYSTEMS`], and one graphics image per accelerator of
/// [`GRAPHICS_ACCELERATORS`] for each operating system that has accelerated images — so a
/// client choosing an image finds one for every platform and every instance family it can
/// launch a fleet or an image builder with, under every release date still published.
fn published_images() -> Vec<PublishedImage> {
    PUBLISHED_RELEASE_DATES
        .iter()
        .flat_map(|released| {
            PUBLISHED_OPERATING_SYSTEMS.iter().flat_map(move |os| {
                let general = PublishedImage {
                    os: *os,
                    accelerator: None,
                    released,
                };
                let graphics = GRAPHICS_ACCELERATORS
                    .iter()
                    .filter(|_| os.graphics)
                    .map(move |accelerator| PublishedImage {
                        os: *os,
                        accelerator: Some(*accelerator),
                        released,
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
