// SPDX-License-Identifier: CC0-1.0

//! The third-party enterprise Linux distributions AWS publishes public AMIs of, beside the
//! Amazon Linux families ([`super::published_amazon_linux`]).
//!
//! ONE ROW PER PUBLISHED RELEASE, VARIANT AND ARCHITECTURE. "The images available to you
//! include public images" (`DescribeImages`), and the public images a fresh account sees are
//! not only Amazon's own distribution: AWS publishes the enterprise Linux distributions it
//! curates — SUSE Linux Enterprise Server, its SAP Applications variant, and Red Hat
//! Enterprise Linux — under the same `amazon` owner alias ("the owner alias (`amazon` |
//! `aws-backup-vault` | `aws-marketplace`)", `Image$ImageOwnerAlias`). That is what a client
//! pinning a non-Amazon-Linux operating system resolves its `ImageId` with: it asks for that
//! owner, the vendor's published name pattern and the architecture it launches on, and takes
//! the newest `CreationDate` of the matches. `DescribeImages` models no error for a filter
//! that selects nothing, so a release this table does not hold is an EMPTY list a client
//! cannot tell from "this Region publishes no such image" — the reason the population of the
//! table is the catalog's contract rather than a convenience.
//!
//! The name forms are the vendors' own published AMI names:
//!
//! ```text
//! suse-sles[-sap]-<release>-v<build date>-hvm-ssd-<architecture>
//! RHEL-<version>_HVM-<build date>-<architecture>-<build>-Hourly2-GP3
//! ```
//!
//! WHICH releases, builds and dates the catalog holds is service-published data that no model
//! describes: it is recorded behaviour, so the tables below carry what is published rather
//! than anything derived. Each row's `CreationDate` is the build date its own name carries, so
//! "the newest match of a name pattern" picks the same image on every run, and each release is
//! published for every architecture the vendor builds it for, so a request that also filters on
//! `architecture` resolves one.
//!
//! No id and no public parameter name is written here: a row is an [`ImageSpec`] of the
//! published catalog ([`super::published_image_specs`]) like every Amazon Linux row, so the
//! ids these images report are minted by the one writer
//! ([`super::ComAmazonawsEc2Amazonec2State::insert_image`]) and are AMIs the account can
//! really describe and launch. These families are discovered by owner and name pattern rather
//! than through the `/aws/service/ami-amazon-linux-latest/` alias hierarchy
//! ([`super::published_amazon_linux`] is the population of that namespace), so no row claims a
//! Systems Manager parameter resolves to it.

use super::images_and_zone_facts::published_amazon_image;
use super::published_amazon_linux::{hvm_boot_mode, ARCHITECTURE_ARM64, ARCHITECTURE_X86_64};
use super::ImageSpec;

/// The architectures a current release of these distributions is published for, in the order
/// the catalog publishes a release's builds in.
const PUBLISHED_ARCHITECTURES: &[&str] = &[ARCHITECTURE_X86_64, ARCHITECTURE_ARM64];

/// The one architecture a variant built for a workload that runs on x86 only is published
/// for.
const X86_64_ONLY: &[&str] = &[ARCHITECTURE_X86_64];

/// How a published description names the architecture a build is for.
fn architecture_description(architecture: &str) -> &'static str {
    if architecture == ARCHITECTURE_ARM64 {
        "64-bit (Arm)"
    } else {
        "64-bit (x86)"
    }
}

/// "The platform details associated with the billing code of the AMI"
/// (`Image$PlatformDetails`) of a SUSE Linux image, and the usage operation that billing code
/// is ([`USAGE_OPERATION_SUSE`]).
///
/// The platform is the one the model's own `CapacityReservationInstancePlatform` publishes for
/// this distribution, which is how a reservation names the same platform an instance launched
/// from one of these images runs on.
const PLATFORM_DETAILS_SUSE: &str = "SUSE Linux";

/// The `UsageOperation` billing code paired with that platform in Amazon's billing-code table
/// (Amazon EC2 User Guide, "Understand AMI billing information").
const USAGE_OPERATION_SUSE: &str = "RunInstances:000g";

/// The same for a Red Hat Enterprise Linux image.
const PLATFORM_DETAILS_RHEL: &str = "Red Hat Enterprise Linux";

/// See [`PLATFORM_DETAILS_RHEL`].
const USAGE_OPERATION_RHEL: &str = "RunInstances:0010";

// ---------------------------------------------------------------------------
// SUSE Linux Enterprise Server: `suse-sles-…`
// ---------------------------------------------------------------------------

/// One SUSE Linux Enterprise Server release the catalog publishes.
struct SlesRelease {
    /// The release as a published AMI name spells it (`16-0`, `15-sp7`).
    name_token: &'static str,
    /// The release as a description spells it (`16.0`, `15 SP7`).
    described: &'static str,
    /// The `v<YYYYMMDD>` build token the published name carries.
    build: &'static str,
    /// `Image$CreationDate` of every build of that release, carrying the build date above so
    /// the name and the date cannot name two different builds.
    creation_date: &'static str,
}

/// The SUSE Linux Enterprise Server releases the catalog publishes: the current major
/// release, and the newest service pack of the previous one — the choice a client resolving
/// this family by name pattern has to make being between releases.
const SLES_RELEASES: &[SlesRelease] = &[
    SlesRelease {
        name_token: "16-0",
        described: "16.0",
        build: "20251104",
        creation_date: "2025-11-04T06:48:12.000Z",
    },
    SlesRelease {
        name_token: "15-sp7",
        described: "15 SP7",
        build: "20250612",
        creation_date: "2025-06-12T05:37:48.000Z",
    },
];

/// The variants each of those releases is published as: `(the token the published name
/// carries, the words the description adds, the architectures it is built for)`.
///
/// The base server is published for every architecture; the SAP Applications variant is
/// published for the architecture its workloads run on.
const SLES_VARIANTS: &[(&str, &str, &[&str])] = &[
    ("", "", PUBLISHED_ARCHITECTURES),
    ("-sap", " for SAP Applications", X86_64_ONLY),
];

/// The published SUSE Linux Enterprise Server builds: every variant of every release in
/// [`SLES_RELEASES`], in each architecture that variant is published for.
fn sles_specs() -> Vec<ImageSpec> {
    let mut specs = Vec::new();
    for release in SLES_RELEASES {
        for &(variant_token, variant_description, architectures) in SLES_VARIANTS {
            for architecture in architectures.iter().copied() {
                let name = format!(
                    "suse-sles{variant_token}-{}-v{}-hvm-ssd-{architecture}",
                    release.name_token, release.build
                );
                specs.push(ImageSpec {
                    description: format!(
                        "SUSE Linux Enterprise Server{variant_description} {} (HVM, {}, \
                         SSD-Backed AMI)",
                        release.described,
                        architecture_description(architecture)
                    ),
                    // A current build of these families boots UEFI on 64-bit Arm and either
                    // firmware on x86, the way every current build of the catalog does.
                    boot_mode: Some(hvm_boot_mode(architecture, true)),
                    // The software on these images is billed beside the instance hours, which
                    // is what their platform details and usage operation report — and what an
                    // instance launched from one reports as its own platform.
                    platform_details: PLATFORM_DETAILS_SUSE.to_string(),
                    usage_operation: USAGE_OPERATION_SUSE.to_string(),
                    ..published_amazon_image(&name, architecture, release.creation_date)
                });
            }
        }
    }
    specs
}

// ---------------------------------------------------------------------------
// Red Hat Enterprise Linux: `RHEL-…`
// ---------------------------------------------------------------------------

/// What every published Red Hat Enterprise Linux build describes itself as: the publisher,
/// which is the whole of `Image$Description` for this family.
const RHEL_DESCRIPTION: &str = "Provided by Red Hat, Inc.";

/// One Red Hat Enterprise Linux release the catalog publishes.
struct RhelRelease {
    /// The release as a published AMI name spells it (`10.0.0`, `9.6.0`).
    version: &'static str,
    /// The `YYYYMMDD` build date the published name carries.
    build: &'static str,
    /// The build number that follows the architecture in the published name — the rebuild of
    /// that release the image is.
    build_number: &'static str,
    /// `Image$CreationDate` of every build of that release, carrying the build date above.
    creation_date: &'static str,
}

/// The Red Hat Enterprise Linux releases the catalog publishes: the current major release and
/// the newest minor release of the previous one, for the same reason [`SLES_RELEASES`] holds
/// two.
const RHEL_RELEASES: &[RhelRelease] = &[
    RhelRelease {
        version: "10.0.0",
        build: "20250513",
        build_number: "0",
        creation_date: "2025-05-13T08:12:37.000Z",
    },
    RhelRelease {
        version: "9.6.0",
        build: "20250618",
        build_number: "0",
        creation_date: "2025-06-18T07:41:52.000Z",
    },
];

/// The published Red Hat Enterprise Linux builds: every release in [`RHEL_RELEASES`], in each
/// published architecture.
fn rhel_specs() -> Vec<ImageSpec> {
    let mut specs = Vec::new();
    for release in RHEL_RELEASES {
        for architecture in PUBLISHED_ARCHITECTURES.iter().copied() {
            let name = format!(
                "RHEL-{}_HVM-{}-{architecture}-{}-Hourly2-GP3",
                release.version, release.build, release.build_number
            );
            specs.push(ImageSpec {
                description: RHEL_DESCRIPTION.to_string(),
                boot_mode: Some(hvm_boot_mode(architecture, true)),
                platform_details: PLATFORM_DETAILS_RHEL.to_string(),
                usage_operation: USAGE_OPERATION_RHEL.to_string(),
                ..published_amazon_image(&name, architecture, release.creation_date)
            });
        }
    }
    specs
}

/// Every third-party distribution build of the published catalog, across each family AWS
/// publishes an AMI of under the `amazon` owner alias.
pub fn published_distribution_specs() -> Vec<ImageSpec> {
    let mut specs = sles_specs();
    specs.extend(rhel_specs());
    specs
}
