// SPDX-License-Identifier: CC0-1.0

//! The published SHAPE of a compute instance: the hardware and platform attributes a
//! Compute Instance offer document carries beside the ones that identify its SKU.
//!
//! RULE: the Price List API publishes, for a compute-instance product, the whole
//! attribute inventory of the instance type and platform it prices — what the instance
//! is built of (processor clock, instruction-set features, architecture, instance store,
//! network and EBS bandwidth, GPU memory, family category) and how its hour is bought
//! (market option, availability zone, the network models it attaches to) — and not only
//! the handful of attributes a price lookup filters on. A client that compares or
//! right-sizes shapes reads those attributes, and each is also a `GetProducts` filter
//! field, so every one of them is published for EVERY (instance type, operating system,
//! Region) the catalog prices.
//!
//! Each value is DERIVED from data about the instance type rather than named per
//! product: the family's published specifications ([`EC2_FAMILY_SHAPES`]), the instance
//! store its sizes carry ([`EC2_INSTANCE_STORE`]), the processor its
//! `physicalProcessor` names ([`PROCESSOR_FEATURES`]) and the normalized size in the
//! type's own name. The NAMES and the per-type specifications are the ones AWS
//! publishes; the figures this module scales by size — an attachment's baseline
//! bandwidth, a size above its family's shared interface — are derived from the family's
//! published bandwidth and the instance's normalized size, exactly as this catalog's
//! rates are its own.

use super::{normalization_size_factor, InstanceTypeSeed};

/// The value the Price List API publishes for an attribute that does not apply to a
/// product.
const NOT_APPLICABLE: &str = "NA";

/// `processorArchitecture`: every instance type this catalog prices runs a 64-bit
/// processor, which is the architecture the API publishes for each of them.
const PROCESSOR_ARCHITECTURE: &str = "64-bit";

/// `marketoption`: these products price capacity bought on demand (and leased under a
/// reservation), which is the market option their offer terms are published under. Spot
/// capacity is a product of its own and this catalog publishes none.
const ON_DEMAND_MARKET_OPTION: &str = "OnDemand";

/// `storage` of an instance type with no instance store: its storage is the EBS volumes
/// attached to it, which the catalog prices as products of their own.
const EBS_ONLY_STORAGE: &str = "EBS only";

/// `vpcnetworkingsupport`: every instance type the catalog prices launches into a VPC.
const VPC_NETWORKING_SUPPORT: &str = "true";

/// The `yes`/`no` attributes as the Price List API spells them.
const PUBLISHED_YES: &str = "Yes";
const PUBLISHED_NO: &str = "No";

/// The normalized size, in hundredths of a normalization unit, of a size the published
/// factor table does not rate — a bare-metal instance, which is its family's largest.
const TOP_SIZE_HUNDREDTHS: u64 = 12_800;

/// The largest normalized size (in hundredths) that SHARES its family's network and EBS
/// bandwidth rather than holding a dedicated share of it: the `4xlarge`-equivalent size,
/// which is where AWS stops publishing an "Up to" figure.
const SHARED_SIZE_HUNDREDTHS: u64 = 3_200;

/// The normalized size (in hundredths) from which an instance holds more than one of its
/// family's interfaces, and so twice its published bandwidth.
const DOUBLE_BANDWIDTH_SIZE_HUNDREDTHS: u64 = 9_600;

/// The normalized size (in hundredths) whose share of its family's burst bandwidth is the
/// whole sustained share: a size at or above it sustains [`SUSTAINED_SHARE_DIVISOR`] of
/// the family's figure, and a smaller one that same fraction of its own size.
const SUSTAINED_SIZE_HUNDREDTHS: u64 = 1_200;

/// The fraction of its family's burst bandwidth that an attachment sustains once its size
/// reaches [`SUSTAINED_SIZE_HUNDREDTHS`]: a third of the burst figure.
const SUSTAINED_SHARE_DIVISOR: u64 = 3;

/// The published shape attributes of one compute instance type, as the
/// `(attribute name, value)` pairs its offer document carries beside the attributes that
/// identify the SKU.
pub(super) fn instance_shape_attributes(
    instance: &InstanceTypeSeed,
) -> Vec<(&'static str, String)> {
    let family = family_shape(instance.instance_type);
    let size = size_hundredths(instance.instance_type);
    let processor = processor_seed(instance.physical_processor);
    let (ebs_throughput, ebs_baseline) = dedicated_ebs_throughput(family, size);
    vec![
        ("availabilityzone", NOT_APPLICABLE.to_string()),
        (
            "classicnetworkingsupport",
            classic_networking_support(instance).to_string(),
        ),
        ("clockSpeed", family.clock_speed.to_string()),
        ("dedicatedEbsThroughput", ebs_throughput),
        ("dedicatedEbsThroughputDescription", ebs_baseline),
        ("ecu", family.ecu.to_string()),
        (
            "enhancedNetworkingSupported",
            yes_no(family.enhanced_networking),
        ),
        ("gpuMemory", family.gpu_memory.to_string()),
        (
            "instanceFamilyCategory",
            title_case(instance.instance_family),
        ),
        ("intelAvx2Available", yes_no(processor.avx2)),
        ("intelAvxAvailable", yes_no(processor.avx)),
        ("intelTurboAvailable", yes_no(processor.turbo)),
        ("marketoption", ON_DEMAND_MARKET_OPTION.to_string()),
        ("networkPerformance", network_performance(family, size)),
        ("processorArchitecture", PROCESSOR_ARCHITECTURE.to_string()),
        ("processorFeatures", processor.features.to_string()),
        ("storage", instance_storage(instance.instance_type).to_string()),
        ("vpcnetworkingsupport", VPC_NETWORKING_SUPPORT.to_string()),
    ]
}

/// A capability attribute as the Price List API writes it: `Yes` or `No`.
fn yes_no(supported: bool) -> String {
    if supported {
        PUBLISHED_YES.to_string()
    } else {
        PUBLISHED_NO.to_string()
    }
}

/// `instanceFamilyCategory`: the category the API names an instance family by, which is
/// the family's own name with each word capitalized (`General purpose` is published as
/// the category `General Purpose`).
fn title_case(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), characters.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// `classicnetworkingsupport`: EC2-Classic carried the previous generation of instance
/// types, so a previous-generation type reports support for it and a current-generation
/// one, which launches only into a VPC, does not.
fn classic_networking_support(instance: &InstanceTypeSeed) -> &'static str {
    if instance.current_generation == PUBLISHED_YES {
        "false"
    } else {
        "true"
    }
}

/// The instance's normalized size in hundredths of a normalization unit, or `None` for a
/// size the published factor table does not rate (a bare-metal instance).
fn size_hundredths(instance_type: &str) -> Option<u64> {
    let factor = normalization_size_factor(instance_type);
    let (whole, fraction) = factor.split_once('.').unwrap_or((factor.as_str(), ""));
    let whole: u64 = whole.parse().ok()?;
    let mut digits: String = fraction.chars().take(2).collect();
    while digits.len() < 2 {
        digits.push('0');
    }
    let fraction: u64 = digits.parse().ok()?;
    whole.checked_mul(100)?.checked_add(fraction)
}

/// `storage`: the instance store of one instance type, or [`EBS_ONLY_STORAGE`] for a type
/// that has none.
fn instance_storage(instance_type: &str) -> &'static str {
    EC2_INSTANCE_STORE
        .iter()
        .find(|(name, _)| *name == instance_type)
        .map_or(EBS_ONLY_STORAGE, |(_, storage)| *storage)
}

/// `networkPerformance` of one size of a family: a family whose bandwidth AWS publishes
/// as a figure reports the bandwidth that size reaches — its family's figure, burstable
/// at the sizes that SHARE the interface and dedicated at the sizes above them — and a
/// family published as a word tier reports the tier its size reaches.
fn network_performance(family: &FamilyShapeSeed, size: Option<u64>) -> String {
    let size = size.unwrap_or(TOP_SIZE_HUNDREDTHS);
    if family.network_gbps == 0 {
        return word_network_tier(size).to_string();
    }
    if size <= SHARED_SIZE_HUNDREDTHS {
        format!("Up to {} Gigabit", family.network_gbps)
    } else if size < DOUBLE_BANDWIDTH_SIZE_HUNDREDTHS {
        format!("{} Gigabit", family.network_gbps)
    } else {
        format!("{} Gigabit", family.network_gbps.saturating_mul(2))
    }
}

/// The word tier a size reaches in a family whose network performance AWS publishes in
/// words rather than as a bandwidth, from the smallest burstable size up to the largest
/// one, which reaches a published figure.
fn word_network_tier(size: u64) -> &'static str {
    match size {
        0..=25 => "Low",
        26..=200 => "Low to Moderate",
        201..=800 => "Moderate",
        801..=SHARED_SIZE_HUNDREDTHS => "High",
        _ => "10 Gigabit",
    }
}

/// `dedicatedEbsThroughput` and the `dedicatedEbsThroughputDescription` that states the
/// baseline beside it.
///
/// A size that shares its family's EBS attachment publishes the family's burst figure as
/// an "Up to" ceiling and its own sustained baseline as the description; a larger size
/// holds a dedicated multiple of that figure and publishes it as both. A family with no
/// dedicated EBS attachment publishes neither figure.
fn dedicated_ebs_throughput(family: &FamilyShapeSeed, size: Option<u64>) -> (String, String) {
    let burst = family.ebs_burst_mbps;
    if burst == 0 {
        return (NOT_APPLICABLE.to_string(), NOT_APPLICABLE.to_string());
    }
    let size = size.unwrap_or(TOP_SIZE_HUNDREDTHS);
    if size <= SHARED_SIZE_HUNDREDTHS {
        // The size's share of the family's burst figure, rounded to the nearest Mbps and
        // capped at the share an attachment sustains.
        let share = (burst.saturating_mul(size) + SUSTAINED_SIZE_HUNDREDTHS / 2)
            / SUSTAINED_SIZE_HUNDREDTHS;
        let baseline = share.min(burst / SUSTAINED_SHARE_DIVISOR);
        (format!("Up to {burst} Mbps"), format!("{baseline} Mbps"))
    } else {
        let dedicated = burst.saturating_mul(size) / SHARED_SIZE_HUNDREDTHS;
        (format!("{dedicated} Mbps"), format!("{dedicated} Mbps"))
    }
}

/// What the Price List API publishes about the instruction-set features of one processor
/// generation.
struct ProcessorSeed {
    /// The marker that identifies the generation inside a `physicalProcessor` value.
    marker: &'static str,
    /// `processorFeatures`: the extensions the API publishes for it, in the order it
    /// publishes them.
    features: &'static str,
    /// `intelAvxAvailable`.
    avx: bool,
    /// `intelAvx2Available`.
    avx2: bool,
    /// `intelTurboAvailable`.
    turbo: bool,
}

impl ProcessorSeed {
    const fn new(
        marker: &'static str,
        features: &'static str,
        avx: bool,
        avx2: bool,
        turbo: bool,
    ) -> Self {
        Self {
            marker,
            features,
            avx,
            avx2,
            turbo,
        }
    }
}

/// The feature sets the API publishes per processor generation, matched by the marker the
/// `physicalProcessor` attribute names the generation by, most specific marker first (a
/// Xeon Platinum part names both `Platinum` and `Xeon`).
const PROCESSOR_FEATURES: &[ProcessorSeed] = &[
    ProcessorSeed::new(
        "Skylake",
        "AVX; AVX2; Intel AVX; Intel AVX2; Intel AVX512; Intel Turbo",
        true,
        true,
        true,
    ),
    ProcessorSeed::new(
        "Ice Lake",
        "AVX; AVX2; Intel AVX; Intel AVX2; Intel AVX512; Intel Turbo",
        true,
        true,
        true,
    ),
    ProcessorSeed::new(
        "Platinum",
        "AVX; AVX2; Intel AVX; Intel AVX2; Intel AVX512; Intel Turbo",
        true,
        true,
        true,
    ),
    ProcessorSeed::new(
        "Broadwell",
        "Intel AVX; Intel AVX2; Intel Turbo",
        true,
        true,
        true,
    ),
    ProcessorSeed::new(
        "Haswell",
        "Intel AVX; Intel AVX2; Intel Turbo",
        true,
        true,
        true,
    ),
    ProcessorSeed::new("Ivy Bridge", "Intel AVX; Intel Turbo", true, false, true),
    // A non-Intel part carries no Intel feature flag, and the API publishes the
    // extensions it does carry.
    ProcessorSeed::new("EPYC", "AVX; AVX2", false, false, false),
    ProcessorSeed::new("Xeon", "Intel AVX; Intel Turbo", true, false, true),
];

/// The feature set published for a processor whose generation names no marker above — a
/// part the API publishes no feature list for.
static UNPUBLISHED_PROCESSOR: ProcessorSeed =
    ProcessorSeed::new("", NOT_APPLICABLE, false, false, false);

/// What the API publishes about the processor a `physicalProcessor` value names.
fn processor_seed(physical_processor: &str) -> &'static ProcessorSeed {
    PROCESSOR_FEATURES
        .iter()
        .find(|seed| physical_processor.contains(seed.marker))
        .unwrap_or(&UNPUBLISHED_PROCESSOR)
}

/// The specifications the Price List API publishes for every size of one instance family.
///
/// They are per FAMILY rather than per size: the processor is the family's, and the
/// bandwidths a size reaches are derived from the family's figure by its normalized size
/// ([`network_performance`], [`dedicated_ebs_throughput`]).
struct FamilyShapeSeed {
    /// The instance-type prefix before the size (`t3` of `t3.nano`).
    family: &'static str,
    /// `clockSpeed`: the frequency the family's processor is published at.
    clock_speed: &'static str,
    /// `ecu`: the family's published compute-unit rating — `Variable` for a burstable
    /// generation and `NA` for a generation the API rates no figure for.
    ecu: &'static str,
    /// The network bandwidth in Gbit/s the family's shared sizes reach, or 0 for a family
    /// whose network performance the API publishes as a word tier.
    network_gbps: u64,
    /// The EBS bandwidth in Mbps the family's shared sizes burst to, or 0 for a family
    /// with no dedicated EBS attachment.
    ebs_burst_mbps: u64,
    /// `gpuMemory`: the memory of the family's GPUs, or `NA` for a family with none.
    gpu_memory: &'static str,
    /// `enhancedNetworkingSupported`: whether the family's instances carry an
    /// enhanced-networking adapter rather than a shared burstable interface.
    enhanced_networking: bool,
}

impl FamilyShapeSeed {
    const fn new(
        family: &'static str,
        clock_speed: &'static str,
        ecu: &'static str,
        network_gbps: u64,
        ebs_burst_mbps: u64,
        gpu_memory: &'static str,
        enhanced_networking: bool,
    ) -> Self {
        Self {
            family,
            clock_speed,
            ecu,
            network_gbps,
            ebs_burst_mbps,
            gpu_memory,
            enhanced_networking,
        }
    }
}

/// The published specifications of every instance family the catalog prices, in the order
/// `EC2_INSTANCE_TYPES` seeds them.
const EC2_FAMILY_SHAPES: &[FamilyShapeSeed] = &[
    FamilyShapeSeed::new("a1", "2.3 GHz", NOT_APPLICABLE, 10, 3_500, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("t2", "Up to 3.3 GHz", "Variable", 0, 0, NOT_APPLICABLE, false),
    FamilyShapeSeed::new("t3", "3.1 GHz", NOT_APPLICABLE, 5, 2_085, NOT_APPLICABLE, false),
    FamilyShapeSeed::new("t3a", "2.5 GHz", NOT_APPLICABLE, 5, 2_085, NOT_APPLICABLE, false),
    FamilyShapeSeed::new("t4g", "2.5 GHz", NOT_APPLICABLE, 5, 2_085, NOT_APPLICABLE, false),
    FamilyShapeSeed::new("m4", "2.4 GHz", NOT_APPLICABLE, 0, 450, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("m5", "3.1 GHz", NOT_APPLICABLE, 10, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("m5a", "2.5 GHz", NOT_APPLICABLE, 10, 2_880, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("m5n", "3.1 GHz", NOT_APPLICABLE, 25, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("m6g", "2.5 GHz", NOT_APPLICABLE, 10, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("m6i", "3.5 GHz", NOT_APPLICABLE, 12, 10_000, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("c1", NOT_APPLICABLE, NOT_APPLICABLE, 0, 0, NOT_APPLICABLE, false),
    FamilyShapeSeed::new("c3", "2.8 GHz", NOT_APPLICABLE, 0, 0, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("c4", "2.9 GHz", NOT_APPLICABLE, 0, 500, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("c5", "3.4 GHz", NOT_APPLICABLE, 10, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("c5a", "3.3 GHz", NOT_APPLICABLE, 10, 3_170, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("c5n", "3.0 GHz", NOT_APPLICABLE, 25, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("c6g", "2.5 GHz", NOT_APPLICABLE, 10, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("c6i", "3.5 GHz", NOT_APPLICABLE, 12, 10_000, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("r4", "2.3 GHz", NOT_APPLICABLE, 10, 3_500, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("r5", "3.1 GHz", NOT_APPLICABLE, 10, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("r6g", "2.5 GHz", NOT_APPLICABLE, 10, 4_750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("x1", "2.3 GHz", NOT_APPLICABLE, 10, 7_000, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("x1e", "2.3 GHz", NOT_APPLICABLE, 10, 3_500, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("z1d", "4.0 GHz", NOT_APPLICABLE, 10, 2_333, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("d2", "2.4 GHz", NOT_APPLICABLE, 0, 750, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("i3", "2.3 GHz", NOT_APPLICABLE, 10, 3_500, NOT_APPLICABLE, true),
    FamilyShapeSeed::new("g4dn", "2.5 GHz", NOT_APPLICABLE, 25, 3_500, "16 GiB", true),
    FamilyShapeSeed::new("p3", "2.3 GHz", NOT_APPLICABLE, 10, 7_000, "16 GiB", true),
];

/// The specifications published for a family the table above does not carry: a type whose
/// family the catalog prices without publishing its hardware still carries every
/// attribute, with no figure claimed for it.
static UNPUBLISHED_FAMILY_SHAPE: FamilyShapeSeed = FamilyShapeSeed::new(
    "",
    NOT_APPLICABLE,
    NOT_APPLICABLE,
    0,
    0,
    NOT_APPLICABLE,
    false,
);

/// The published specifications of the family an instance type belongs to.
fn family_shape(instance_type: &str) -> &'static FamilyShapeSeed {
    let family = instance_type
        .split_once('.')
        .map_or(instance_type, |(family, _size)| family);
    EC2_FAMILY_SHAPES
        .iter()
        .find(|seed| seed.family == family)
        .unwrap_or(&UNPUBLISHED_FAMILY_SHAPE)
}

/// The instance store the Price List API publishes for the instance types that carry one,
/// as `(instance type, storage)` — the device count, the size of each device and its
/// medium, exactly as the `storage` attribute states them. Every other type the catalog
/// prices is [`EBS_ONLY_STORAGE`].
const EC2_INSTANCE_STORE: &[(&str, &str)] = &[
    ("c1.medium", "1 x 350"),
    ("c1.xlarge", "4 x 420"),
    ("c3.large", "2 x 16 SSD"),
    ("c3.xlarge", "2 x 40 SSD"),
    ("c3.2xlarge", "2 x 80 SSD"),
    ("c3.4xlarge", "2 x 160 SSD"),
    ("c3.8xlarge", "2 x 320 SSD"),
    ("d2.xlarge", "3 x 2000 HDD"),
    ("d2.2xlarge", "6 x 2000 HDD"),
    ("d2.4xlarge", "12 x 2000 HDD"),
    ("d2.8xlarge", "24 x 2000 HDD"),
    ("g4dn.xlarge", "1 x 125 NVMe SSD"),
    ("g4dn.2xlarge", "1 x 225 NVMe SSD"),
    ("g4dn.4xlarge", "1 x 225 NVMe SSD"),
    ("g4dn.8xlarge", "1 x 900 NVMe SSD"),
    ("g4dn.12xlarge", "1 x 900 NVMe SSD"),
    ("g4dn.16xlarge", "1 x 900 NVMe SSD"),
    ("i3.large", "1 x 475 NVMe SSD"),
    ("i3.xlarge", "1 x 950 NVMe SSD"),
    ("i3.2xlarge", "1 x 1900 NVMe SSD"),
    ("i3.4xlarge", "2 x 1900 NVMe SSD"),
    ("i3.8xlarge", "4 x 1900 NVMe SSD"),
    ("i3.16xlarge", "8 x 1900 NVMe SSD"),
    ("i3.metal", "8 x 1900 NVMe SSD"),
    ("x1.16xlarge", "1 x 1920 SSD"),
    ("x1.32xlarge", "2 x 1920 SSD"),
    ("x1e.xlarge", "1 x 120 SSD"),
    ("x1e.2xlarge", "1 x 240 SSD"),
    ("x1e.4xlarge", "1 x 480 SSD"),
    ("x1e.8xlarge", "1 x 960 SSD"),
    ("x1e.16xlarge", "1 x 1920 SSD"),
    ("x1e.32xlarge", "2 x 1920 SSD"),
    ("z1d.large", "1 x 75 NVMe SSD"),
    ("z1d.xlarge", "1 x 150 NVMe SSD"),
    ("z1d.2xlarge", "1 x 300 NVMe SSD"),
    ("z1d.3xlarge", "1 x 450 NVMe SSD"),
    ("z1d.6xlarge", "1 x 900 NVMe SSD"),
    ("z1d.12xlarge", "2 x 900 NVMe SSD"),
    ("z1d.metal", "2 x 900 NVMe SSD"),
];
