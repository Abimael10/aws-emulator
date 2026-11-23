// SPDX-License-Identifier: CC0-1.0

//! The operating system inside a managed node's GUEST, as the node reports it:
//! `InstanceInformation$PlatformName` ("the name of the operating system platform running on
//! your managed node") and `$PlatformVersion` ("the version of the OS platform running on
//! your managed node").
//!
//! # Where the pair comes from
//!
//! The agent reads both from inside the guest, out of the operating system's OWN release
//! metadata — the `NAME` and `VERSION_ID` of the os-release file on a Linux node, the edition
//! and NT version on a Windows one. Nothing in this emulator enters a guest to ask, so the
//! pair is derived from the one record that says which operating system the guest holds: the
//! Amazon EC2 image the instance was launched from. That is the same record this service
//! already reads which distribution a node is patched against off
//! ([`super::patch_runs::DISTRIBUTION_IMAGE_NAMES`]) and which release a patch snapshot names
//! off ([`super::patch_snapshots::node_product`]), so the three read one fact about the guest.
//! The table here is its own because what it carries is the operating system's own release
//! metadata rather than the modelled `OperatingSystem` a baseline is keyed by: the two are
//! different catalogs of the same families, and a family one of them knows nothing of is
//! answered by that reader's own fallback.
//!
//! An image name is a published fact of each family rather than anything a model describes,
//! so [`LINUX_FAMILIES`] and [`WINDOWS_RELEASES`] carry what each vendor publishes: the
//! `NAME`/`VERSION_ID` its own os-release file carries, and — for Windows Server — the NT
//! version Microsoft publishes for that release ("Windows Server release information",
//! Microsoft Learn).
//!
//! An image whose name names no family below reports NEITHER member, and a family whose
//! release the name does not spell reports the name alone: a member nothing can be read for
//! is left unset rather than invented, which is how a node that has reported nothing for a
//! member is served ([`super::ManagedNodeRecord::to_instance_information`]).

/// What the guest of a node runs, as the node reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestPlatform {
    /// `InstanceInformation$PlatformName`: the `NAME` the operating system's own release
    /// metadata carries.
    pub name: String,
    /// `InstanceInformation$PlatformVersion`: the release that metadata carries, or `None`
    /// for an image whose name spells no release of its family.
    pub version: Option<String>,
}

/// Where the release a family's node reports comes from.
#[derive(Debug, Clone, Copy)]
enum Release {
    /// The family is one release, whose version its images' names do not vary: Amazon Linux
    /// 2's os-release carries `VERSION_ID="2"` whichever build an instance was launched from.
    Published(&'static str),
    /// The release is the first version the image NAME spells, kept to that many
    /// dot-separated components — the form that family's own `VERSION_ID` carries
    /// (`RHEL-9.6.0_HVM-…` is `9.6`).
    InImage(usize),
    /// The release is the major version and service pack a SUSE image name spells
    /// (`suse-sles-15-sp7-…`), which SUSE's `VERSION_ID` joins with a dot (`15.7`).
    ServicePack,
}

/// One operating-system family an image name names.
#[derive(Debug)]
struct GuestFamily {
    /// The prefix an image name of this family carries, compared case-insensitively — the
    /// longest matching prefix wins, so `amzn2` is not read as `amzn`.
    image_name_prefix: &'static str,
    /// The `NAME` this family's os-release file carries, which is what its nodes report as
    /// `PlatformName`.
    platform_name: &'static str,
    /// Where its nodes' `PlatformVersion` comes from.
    release: Release,
}

/// The Linux families an image name can name, each with the `NAME` and `VERSION_ID` its own
/// os-release file carries.
///
/// The Amazon Linux rows are the families whose images this emulator publishes
/// (`service_com_amazonaws_ec2_amazonec2::state::published_amazon_linux_specs`); the rows
/// after them are the distributions AWS publishes beside them and the ones a client registers
/// an image of its own under, named as those projects publish their images. Amazon Linux's
/// own os-release renamed itself after its first family — `NAME="Amazon Linux AMI"` with
/// `VERSION_ID="2018.03"` on the original, `NAME="Amazon Linux"` with the bare family number
/// on every release since.
///
/// Each prefix carries the segment that identifies the FAMILY, not only its first word, so an
/// image of another kind published under the same first word — Amazon's macOS images, whose
/// names open `amzn-ec2-macos-` — names no family here and reports no platform at all rather
/// than a plausible wrong one.
const LINUX_FAMILIES: &[GuestFamily] = &[
    GuestFamily {
        image_name_prefix: "al2023-ami",
        platform_name: "Amazon Linux",
        release: Release::Published("2023"),
    },
    GuestFamily {
        image_name_prefix: "al2027-preview-ami",
        platform_name: "Amazon Linux",
        release: Release::Published("2027"),
    },
    GuestFamily {
        image_name_prefix: "amzn2-ami",
        platform_name: "Amazon Linux",
        release: Release::Published("2"),
    },
    GuestFamily {
        image_name_prefix: "amzn-ami",
        platform_name: "Amazon Linux AMI",
        release: Release::Published("2018.03"),
    },
    GuestFamily {
        image_name_prefix: "rhel",
        platform_name: "Red Hat Enterprise Linux",
        release: Release::InImage(2),
    },
    GuestFamily {
        image_name_prefix: "suse-sles",
        platform_name: "SLES",
        release: Release::ServicePack,
    },
    GuestFamily {
        image_name_prefix: "ubuntu",
        platform_name: "Ubuntu",
        release: Release::InImage(2),
    },
    GuestFamily {
        image_name_prefix: "debian",
        platform_name: "Debian GNU/Linux",
        release: Release::InImage(1),
    },
    GuestFamily {
        image_name_prefix: "centos",
        platform_name: "CentOS Linux",
        release: Release::InImage(1),
    },
    GuestFamily {
        image_name_prefix: "rocky",
        platform_name: "Rocky Linux",
        release: Release::InImage(2),
    },
    GuestFamily {
        image_name_prefix: "almalinux",
        platform_name: "AlmaLinux",
        release: Release::InImage(2),
    },
];

/// The token a Windows Server image name carries after `Windows_Server-`, and what a node
/// launched from a base image of that release reports: the edition those images run, and the
/// NT version Microsoft publishes for the release.
///
/// The releases are the ones AWS publishes an `ami-windows-latest` image of, and the base
/// images of each are the Datacenter edition.
const WINDOWS_RELEASES: &[(&str, &str, &str)] = &[
    (
        "2025",
        "Microsoft Windows Server 2025 Datacenter",
        "10.0.26100",
    ),
    (
        "2022",
        "Microsoft Windows Server 2022 Datacenter",
        "10.0.20348",
    ),
    (
        "2019",
        "Microsoft Windows Server 2019 Datacenter",
        "10.0.17763",
    ),
    (
        "2016",
        "Microsoft Windows Server 2016 Datacenter",
        "10.0.14393",
    ),
    (
        "2012-r2",
        "Microsoft Windows Server 2012 R2 Datacenter",
        "6.3.9600",
    ),
    (
        "2012",
        "Microsoft Windows Server 2012 Datacenter",
        "6.2.9200",
    ),
];

/// How a Windows Server image name introduces the release it is a build of.
const WINDOWS_NAME_PREFIX: &str = "windows_server-";

/// The token a SUSE image name carries for the variant built for SAP workloads, which stands
/// between the family and the release.
const SUSE_SAP_VARIANT: &str = "-sap";

/// The characters an image name separates its segments with, which is what makes a segment a
/// version rather than a run of digits inside a word (`gp3`, `x86_64`, `ec2`).
const NAME_SEPARATORS: &[char] = &['-', '_', '/', ' '];

/// How many digits the first component of a release may carry: a published release numbers its
/// family (`9`, `24`, `2023`), while the longer run of digits a name carries is the `YYYYMMDD`
/// build date of that build — which is not a version of anything, so a name that spells only
/// one reports no version at all.
const MAX_RELEASE_COMPONENT_DIGITS: usize = 4;

/// What the guest of a node launched from the image named `image_name` runs, on a node whose
/// `PlatformType` is `platform_type` — or `None` for an image whose name names no family this
/// reads, which includes every image published under no name at all.
///
/// A Windows node's platform is read off the Windows Server release its image names
/// ([`WINDOWS_RELEASES`]) and every other node's off its Linux family ([`LINUX_FAMILIES`]),
/// because the two name their releases in their own forms; which of the two a node is on is
/// Amazon EC2's own `Image$Platform` as this service already reads it
/// ([`super::node_registration`]).
#[must_use]
pub fn guest_platform(platform_type: &str, image_name: Option<&str>) -> Option<GuestPlatform> {
    let image_name = image_name?.to_ascii_lowercase();
    if platform_type == super::node_registration::PLATFORM_TYPE_WINDOWS {
        return windows_platform(&image_name);
    }
    let family = LINUX_FAMILIES
        .iter()
        .filter(|family| image_name.starts_with(family.image_name_prefix))
        .max_by_key(|family| family.image_name_prefix.len())?;
    Some(GuestPlatform {
        name: family.platform_name.to_string(),
        version: match family.release {
            Release::Published(version) => Some(version.to_string()),
            Release::InImage(components) => release_in_name(&image_name, components),
            Release::ServicePack => service_pack_release(&image_name),
        },
    })
}

/// The platform of a node whose image is a Windows Server build, read off the release its
/// name carries — the longest release token the name introduces, so `2012-r2` is not read as
/// `2012`.
fn windows_platform(image_name: &str) -> Option<GuestPlatform> {
    let (_, platform_name, nt_version) = WINDOWS_RELEASES
        .iter()
        .filter(|(token, _, _)| image_name.contains(&format!("{WINDOWS_NAME_PREFIX}{token}")))
        .max_by_key(|(token, _, _)| token.len())?;
    Some(GuestPlatform {
        name: (*platform_name).to_string(),
        version: Some((*nt_version).to_string()),
    })
}

/// The release `image_name` spells, kept to its first `components` dot-separated parts, or
/// `None` when the name spells none.
///
/// A release is a whole SEGMENT of the name ([`NAME_SEPARATORS`]) made of digits and dots, so
/// the digits inside a word — the `3` of `gp3`, the `2` of `ec2`, the `86` of `x86_64` — are
/// not read as one, and its first component is a family number rather than a build date
/// ([`MAX_RELEASE_COMPONENT_DIGITS`]).
fn release_in_name(image_name: &str, components: usize) -> Option<String> {
    image_name
        .split(NAME_SEPARATORS)
        .filter(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
        })
        .map(|segment| {
            segment
                .split('.')
                .filter(|part| !part.is_empty())
                .collect::<Vec<&str>>()
        })
        .find(|parts| {
            parts.len() >= components
                && parts
                    .first()
                    .is_some_and(|major| major.len() <= MAX_RELEASE_COMPONENT_DIGITS)
        })
        .map(|parts| {
            parts
                .into_iter()
                .take(components)
                .collect::<Vec<&str>>()
                .join(".")
        })
}

/// The release a SUSE image name spells, in the form SUSE's own `VERSION_ID` carries: the
/// major version and the service pack, joined with a dot (`15-sp7` is `15.7`, `16-0` is
/// `16.0`).
///
/// The release is the part of the name between the family — with the workload variant a
/// build is of, which follows it — and the `v<build date>` every build of that family
/// carries.
fn service_pack_release(image_name: &str) -> Option<String> {
    let rest = image_name.strip_prefix("suse-sles")?;
    let rest = rest.strip_prefix(SUSE_SAP_VARIANT).unwrap_or(rest);
    let release = rest.strip_prefix('-')?;
    let release = release.split("-v").next()?;
    let mut parts = release.split('-');
    let major = parts.next().filter(|major| !major.is_empty())?;
    let service_pack = parts.next().unwrap_or("0");
    let service_pack = service_pack.trim_start_matches("sp");
    (!service_pack.is_empty()).then(|| format!("{major}.{service_pack}"))
}
