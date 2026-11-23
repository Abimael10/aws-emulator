// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for three more families — the `e` variant
//! of the sixth generation x86 graphics family, the network-optimized memory family with local
//! NVMe devices, and the first generation training-accelerated family — driven over the wire
//! this service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with
//! XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one `published_g6_graphics_rows` pins for the family beside the
//! first of these: EVERY MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE TYPE IS THE
//! FACT AWS PUBLISHES FOR THAT ONE TYPE, read off a per-type table — never a function of the
//! family letter and the size multiplier in its name. So a type describes the accelerators it
//! carries (`InstanceTypeInfo$GpuInfo`, `$NeuronInfo`) and none at all where it carries none,
//! the EBS bandwidth and IOPS it sustains beside the figures it reaches
//! (`EbsInfo$EbsOptimizedInfo`), how many volumes it holds attached and whether that limit is
//! the volumes' own (`$MaximumEbsAttachments` / `$AttachmentLimitType`), its local devices
//! (`InstanceStorageInfo`), its network column and the card that carries it
//! (`NetworkInfo$NetworkPerformance` / `$NetworkCards`), its processor's maker and sustained
//! frequency (`ProcessorInfo`), the core counts a caller may configure (`VCpuInfo$ValidCores`)
//! and the adapters, usage classes, boot modes and host features it is offered.
//!
//! The tables below span what the rule generalises over: three families of three different
//! designs, every size of two of them and both sizes of the third, types with GPUs beside types
//! with Neuron devices and types with neither, memory ratios of four, eight and sixteen GiB per
//! vCPU, local stores of one, two and four devices beside the one size of these families that
//! has none, EBS shares that burst above what they sustain beside dedicated ones, volume limits
//! of five distinct figures over two kinds, and — for every capability member — one type that
//! has it beside one that does not. The closing test holds two families at ONE size against each
//! other, so no row of these tables can be read as the specification.
//!
//! The figures are the ones AWS states for each type: each family's own instance-type
//! specification page (accelerators, vCPUs, memory, local devices, network performance), the
//! "Amazon EBS optimized instance types" table (the EBS figures), "Instance volume limits" (the
//! attachment limit and its kind), the "IP addresses per network interface per instance type"
//! table (the interface limits) and the published lists of the types offered an Elastic Fabric
//! Adapter, ENA Express, hibernation, Capacity Blocks and Dedicated Hosts.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    assert_published_ebs, described_instance_type, emulator, reported_instance_type_list,
    reported_instance_type_member,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "eu-west-1";

/// The MiB in a GiB, for reading a published memory figure stated in GiB.
const MIB_PER_GIB: i64 = 1_024;

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// One member of the reported type, or a panic naming the case when the type reports none.
fn required(case: &str, body: &str, path: &[&str]) -> String {
    reported_instance_type_member(case, body, path)
        .unwrap_or_else(|| panic!("case {case:?}: no {path:?} reported, got {body}"))
}

/// One member of the reported type as a number.
fn number(case: &str, body: &str, path: &[&str]) -> f64 {
    let text = required(case, body, path);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {path:?} must be a number, got {text:?}"))
}

/// The published hardware row of one instance type: the figures a client sizes its memory, its
/// volumes, its storage, its network and its processor from.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `g6e.12xlarge` and the like.
    instance_type: &'static str,
    /// Its memory, in GiB as its specification page states it.
    memory_gib: i64,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// How many Amazon EBS volumes it holds attached, and whether that limit is the volumes'
    /// own (`dedicated`) or shared with its interfaces and local devices (`shared`).
    ebs_attachments: (i64, &'static str),
    /// Its local devices, as (count, GB each).
    instance_store: (i64, i64),
    /// "The network performance", as the published table writes that column.
    network_performance: &'static str,
    /// Its processor's maker and that processor's sustained frequency, in GHz.
    processor: (&'static str, f64),
}

/// Every size of the three families, with the figures AWS publishes for each.
const ROWS: &[Row] = &[
    // The graphics family's `e` variant: 8 GiB of memory per vCPU, the EBS ladder its sibling
    // family reads, local devices of its own sizes, and a network column well above a general
    // purpose host's.
    Row {
        instance_type: "g6e.xlarge",
        memory_gib: 32,
        ebs: (1_000, 4_000, 5_000, 20_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: (1, 250),
        network_performance: "Up to 20 Gigabit",
        processor: ("AMD", 3.4),
    },
    Row {
        instance_type: "g6e.2xlarge",
        memory_gib: 64,
        ebs: (2_000, 8_000, 5_000, 20_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: (1, 450),
        network_performance: "Up to 20 Gigabit",
        processor: ("AMD", 3.4),
    },
    Row {
        instance_type: "g6e.4xlarge",
        memory_gib: 128,
        ebs: (8_000, 32_000, 8_000, 32_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: (1, 600),
        network_performance: "20 Gigabit",
        processor: ("AMD", 3.4),
    },
    Row {
        instance_type: "g6e.8xlarge",
        memory_gib: 256,
        ebs: (16_000, 64_000, 16_000, 64_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: (2, 450),
        network_performance: "25 Gigabit",
        processor: ("AMD", 3.4),
    },
    Row {
        instance_type: "g6e.12xlarge",
        memory_gib: 384,
        ebs: (20_000, 80_000, 20_000, 80_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: (2, 1_900),
        network_performance: "100 Gigabit",
        processor: ("AMD", 3.4),
    },
    Row {
        instance_type: "g6e.16xlarge",
        memory_gib: 512,
        ebs: (20_000, 80_000, 20_000, 80_000),
        ebs_attachments: (48, "dedicated"),
        instance_store: (2, 950),
        network_performance: "35 Gigabit",
        processor: ("AMD", 3.4),
    },
    Row {
        instance_type: "g6e.24xlarge",
        memory_gib: 768,
        ebs: (30_000, 120_000, 30_000, 120_000),
        ebs_attachments: (64, "dedicated"),
        instance_store: (2, 1_900),
        network_performance: "200 Gigabit",
        processor: ("AMD", 3.4),
    },
    Row {
        instance_type: "g6e.48xlarge",
        memory_gib: 1_536,
        ebs: (60_000, 240_000, 60_000, 240_000),
        ebs_attachments: (128, "dedicated"),
        instance_store: (4, 1_900),
        network_performance: "400 Gigabit",
        processor: ("AMD", 3.4),
    },
    // The network-optimized memory family: 16 GiB of memory per vCPU, its host's EBS figures
    // and local devices, and a 100 Gigabit network its narrow sizes burst into.
    Row {
        instance_type: "r5dn.large",
        memory_gib: 16,
        ebs: (650, 3_600, 4_750, 18_750),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 75),
        network_performance: "Up to 25 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.xlarge",
        memory_gib: 32,
        ebs: (1_150, 6_000, 4_750, 18_750),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 150),
        network_performance: "Up to 25 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.2xlarge",
        memory_gib: 64,
        ebs: (2_300, 12_000, 4_750, 18_750),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 300),
        network_performance: "Up to 25 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.4xlarge",
        memory_gib: 128,
        ebs: (4_750, 18_750, 4_750, 18_750),
        ebs_attachments: (25, "shared"),
        instance_store: (2, 300),
        network_performance: "Up to 25 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.8xlarge",
        memory_gib: 256,
        ebs: (6_800, 30_000, 6_800, 30_000),
        ebs_attachments: (25, "shared"),
        instance_store: (2, 600),
        network_performance: "25 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.12xlarge",
        memory_gib: 384,
        ebs: (9_500, 40_000, 9_500, 40_000),
        ebs_attachments: (25, "shared"),
        instance_store: (2, 900),
        network_performance: "50 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.16xlarge",
        memory_gib: 512,
        ebs: (13_600, 60_000, 13_600, 60_000),
        ebs_attachments: (23, "shared"),
        instance_store: (4, 600),
        network_performance: "75 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.24xlarge",
        memory_gib: 768,
        ebs: (19_000, 80_000, 19_000, 80_000),
        ebs_attachments: (23, "shared"),
        instance_store: (4, 900),
        network_performance: "100 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5dn.metal",
        memory_gib: 768,
        ebs: (19_000, 80_000, 19_000, 80_000),
        ebs_attachments: (31, "shared"),
        instance_store: (4, 900),
        network_performance: "100 Gigabit",
        processor: ("Intel", 3.1),
    },
    // The training-accelerated family: 4 GiB of memory per vCPU, the published EBS figures of a
    // whole accelerated host and a sixteenth of them on the narrow size, and a network of eight
    // cards on the size that holds the host.
    Row {
        instance_type: "trn1.2xlarge",
        memory_gib: 32,
        ebs: (5_000, 16_250, 20_000, 65_000),
        ebs_attachments: (25, "shared"),
        instance_store: (1, 475),
        network_performance: "Up to 12.5 Gigabit",
        processor: ("Intel", 3.5),
    },
    Row {
        instance_type: "trn1.32xlarge",
        memory_gib: 512,
        ebs: (80_000, 260_000, 80_000, 260_000),
        ebs_attachments: (28, "shared"),
        instance_store: (4, 1_900),
        network_performance: "800 Gigabit",
        processor: ("Intel", 3.5),
    },
];

/// Rule, row by row: every member is the figure AWS publishes for THAT instance type.
#[tokio::test]
async fn every_published_row_of_the_three_families_is_reported_for_its_own_type() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let reported = number(case, &body, &["memoryInfo", "sizeInMiB"]) as i64;
        let expected = row.memory_gib * MIB_PER_GIB;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected memoryInfo.sizeInMiB {expected}, got {reported}"
        );

        let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = row.ebs;
        // The six readings of one published row, through the shared reader so the Mbps and the
        // MB/s of one figure cannot be asserted inconsistently.
        assert_published_ebs(
            case,
            &body,
            (
                baseline_mbps as f64,
                baseline_iops as f64,
                maximum_mbps as f64,
                maximum_iops as f64,
            ),
        );

        let (attachments, limit_kind) = row.ebs_attachments;
        let reported = number(case, &body, &["ebsInfo", "maximumEbsAttachments"]) as i64;
        assert_eq!(
            reported, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {reported}"
        );
        let reported = required(case, &body, &["ebsInfo", "attachmentLimitType"]);
        assert_eq!(
            reported, limit_kind,
            "case {case:?}: expected attachmentLimitType {limit_kind:?}, got {reported:?}"
        );

        let (devices, gb_each) = row.instance_store;
        let reported =
            number(case, &body, &["instanceStorageInfo", "disks", "item", "count"]) as i64;
        assert_eq!(
            reported, devices,
            "case {case:?}: expected {devices} local devices, got {reported}"
        );
        let reported =
            number(case, &body, &["instanceStorageInfo", "disks", "item", "sizeInGB"]) as i64;
        assert_eq!(
            reported, gb_each,
            "case {case:?}: expected local devices of {gb_each} GB, got {reported}"
        );
        // The total is one device's size times the count, so the two readings of one instance
        // store cannot disagree.
        let expected = devices * gb_each;
        let reported = number(case, &body, &["instanceStorageInfo", "totalSizeInGB"]) as i64;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected totalSizeInGB {expected}, got {reported}"
        );

        let reported = required(case, &body, &["networkInfo", "networkPerformance"]);
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );

        let (manufacturer, sustained_ghz) = row.processor;
        let reported = required(case, &body, &["processorInfo", "manufacturer"]);
        assert_eq!(
            reported, manufacturer,
            "case {case:?}: expected processor manufacturer {manufacturer:?}, got {reported:?}"
        );
        let reported = number(case, &body, &["processorInfo", "sustainedClockSpeedInGhz"]);
        assert!(
            (reported - sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {sustained_ghz}, got {reported}"
        );
    }
}

/// Rule: the accelerators a type carries are published for that type — a GPU family describes
/// GPUs, a Neuron family describes Neuron devices with the cores one device carries, and a type
/// that carries neither describes neither.
#[tokio::test]
async fn published_accelerator_inventories_are_facts_of_the_type() {
    let (router, _engine) = emulator();

    // Rows: the GPU count changes up one ladder without following its size multiplier (one
    // accelerator on four sizes, four on two, eight on the widest), and the Neuron count the
    // same way.
    let gpu_rows = [
        ("g6e.xlarge", 1_i64),
        ("g6e.8xlarge", 1),
        ("g6e.12xlarge", 4),
        ("g6e.16xlarge", 1),
        ("g6e.24xlarge", 4),
        ("g6e.48xlarge", 8),
    ];
    for (instance_type, count) in gpu_rows {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = number(case, &body, &["gpuInfo", "gpus", "item", "count"]) as i64;
        assert_eq!(
            reported, count,
            "case {case:?}: expected {count} accelerators, got {reported}"
        );
        let reported = required(case, &body, &["gpuInfo", "gpus", "item", "name"]);
        assert_eq!(
            reported, "L40S",
            "case {case:?}: expected the family's accelerator, got {reported:?}"
        );
        let reported = required(case, &body, &["gpuInfo", "gpus", "item", "manufacturer"]);
        assert_eq!(
            reported, "NVIDIA",
            "case {case:?}: expected the accelerator's maker, got {reported:?}"
        );
        let per_device = number(
            case,
            &body,
            &["gpuInfo", "gpus", "item", "memoryInfo", "sizeInMiB"],
        ) as i64;
        assert_eq!(
            per_device, 45_776,
            "case {case:?}: expected 45776 MiB per accelerator, got {per_device}"
        );
        // The total is one device's memory times the count, so the two readings of one
        // inventory cannot disagree.
        let expected = per_device * count;
        let reported = number(case, &body, &["gpuInfo", "totalGpuMemoryInMiB"]) as i64;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected totalGpuMemoryInMiB {expected}, got {reported}"
        );
        // A GPU family carries no Neuron device, which it describes as nothing at all.
        let reported = reported_instance_type_member(
            case,
            &body,
            &["neuronInfo", "totalNeuronDeviceMemoryInMiB"],
        );
        assert!(
            reported.is_none(),
            "case {case:?}: a GPU type describes no Neuron device, got {reported:?}"
        );
    }

    // And the Neuron family: one device on the narrow size, the host's whole sixteen on the
    // size that holds it, each carrying two cores of the second version.
    for (instance_type, count) in [("trn1.2xlarge", 1_i64), ("trn1.32xlarge", 16)] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = number(
            case,
            &body,
            &["neuronInfo", "neuronDevices", "item", "count"],
        ) as i64;
        assert_eq!(
            reported, count,
            "case {case:?}: expected {count} Neuron devices, got {reported}"
        );
        let reported = required(case, &body, &["neuronInfo", "neuronDevices", "item", "name"]);
        assert_eq!(
            reported, "Trainium",
            "case {case:?}: expected the family's Neuron device, got {reported:?}"
        );
        for (member, expected) in [("count", 2_i64), ("version", 2)] {
            let reported = number(
                case,
                &body,
                &["neuronInfo", "neuronDevices", "item", "coreInfo", member],
            ) as i64;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected coreInfo.{member} {expected}, got {reported}"
            );
        }
        let per_device = number(
            case,
            &body,
            &["neuronInfo", "neuronDevices", "item", "memoryInfo", "sizeInMiB"],
        ) as i64;
        assert_eq!(
            per_device, 32_768,
            "case {case:?}: expected 32768 MiB per Neuron device, got {per_device}"
        );
        let expected = per_device * count;
        let reported = number(case, &body, &["neuronInfo", "totalNeuronDeviceMemoryInMiB"]) as i64;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected totalNeuronDeviceMemoryInMiB {expected}, got {reported}"
        );
        // The family publishes its devices as Neuron devices alone, with no Inference
        // accelerator inventory of the same hardware beside them.
        let reported = reported_instance_type_member(
            case,
            &body,
            &["inferenceAcceleratorInfo", "totalInferenceMemoryInMiB"],
        );
        assert!(
            reported.is_none(),
            "case {case:?}: this family describes no Inference accelerator, got {reported:?}"
        );
    }

    // A type of the network-optimized memory family carries neither kind of accelerator, so it
    // describes neither — the other half of the same rule.
    for case in ["r5dn.large", "r5dn.24xlarge", "r5dn.metal"] {
        let body = described(case, &router, case).await;
        for path in [
            vec!["gpuInfo", "totalGpuMemoryInMiB"],
            vec!["neuronInfo", "totalNeuronDeviceMemoryInMiB"],
        ] {
            let reported = reported_instance_type_member(case, &body, &path);
            assert!(
                reported.is_none(),
                "case {case:?}: a type with no accelerator describes no {path:?}, got {reported:?}"
            );
        }
    }
}

/// Rule: the adapters, usage classes, boot modes and host features a type is offered are
/// published per type, so neither a hardware generation nor a family answers for every size on
/// it.
#[tokio::test]
async fn published_capabilities_are_reported_per_instance_type() {
    let (router, _engine) = emulator();

    // For each member, one type that has the capability beside one that does not.
    let rows = [
        // The Elastic Fabric Adapter: offered from the size that holds a dedicated network
        // share up on the graphics family, on the two sizes that hold a whole host on the
        // memory family — and NOT on the size below them — and on the accelerated host alone.
        ("g6e.4xlarge", vec!["networkInfo", "efaSupported"], "false"),
        ("g6e.8xlarge", vec!["networkInfo", "efaSupported"], "true"),
        ("r5dn.16xlarge", vec!["networkInfo", "efaSupported"], "false"),
        ("r5dn.24xlarge", vec!["networkInfo", "efaSupported"], "true"),
        ("r5dn.metal", vec!["networkInfo", "efaSupported"], "true"),
        ("trn1.2xlarge", vec!["networkInfo", "efaSupported"], "false"),
        ("trn1.32xlarge", vec!["networkInfo", "efaSupported"], "true"),
        (
            "trn1.32xlarge",
            vec!["networkInfo", "efaInfo", "maximumEfaInterfaces"],
            "8",
        ),
        (
            "r5dn.24xlarge",
            vec!["networkInfo", "efaInfo", "maximumEfaInterfaces"],
            "1",
        ),
        // ENA Express: the graphics sizes that carry several accelerators and the adapter that
        // goes with them, and no size of the fifth generation memory family.
        (
            "g6e.16xlarge",
            vec!["networkInfo", "enaSrdSupported"],
            "false",
        ),
        (
            "g6e.24xlarge",
            vec!["networkInfo", "enaSrdSupported"],
            "true",
        ),
        (
            "g6e.48xlarge",
            vec!["networkInfo", "enaSrdSupported"],
            "true",
        ),
        (
            "r5dn.large",
            vec!["networkInfo", "enaSrdSupported"],
            "false",
        ),
        // The cards the interfaces are attached to: one on the narrower sizes, and the several
        // a published network of more than one adapter's bandwidth is spread over.
        ("g6e.12xlarge", vec!["networkInfo", "maximumNetworkCards"], "1"),
        ("g6e.48xlarge", vec!["networkInfo", "maximumNetworkCards"], "4"),
        ("r5dn.large", vec!["networkInfo", "maximumNetworkCards"], "1"),
        (
            "trn1.32xlarge",
            vec!["networkInfo", "maximumNetworkCards"],
            "8",
        ),
        (
            "trn1.32xlarge",
            vec!["networkInfo", "defaultNetworkCardIndex"],
            "0",
        ),
        // In-transit encryption between instances: offered on the two families whose published
        // lists name them.
        (
            "r5dn.large",
            vec!["networkInfo", "encryptionInTransitSupported"],
            "true",
        ),
        (
            "trn1.2xlarge",
            vec!["networkInfo", "encryptionInTransitSupported"],
            "true",
        ),
        // Hibernation: not offered on the network-optimized memory family at any size,
        // whatever its memory, while its sibling without that capability letter is offered it
        // at the sizes under the published memory bound.
        ("r5dn.large", vec!["hibernationSupported"], "false"),
        ("r5dn.4xlarge", vec!["hibernationSupported"], "false"),
        ("r5d.large", vec!["hibernationSupported"], "true"),
        ("g6e.xlarge", vec!["hibernationSupported"], "false"),
        // Dedicated Hosts: offered on the graphics sizes that hold at least half the host's
        // accelerators and not on the narrower ones.
        ("g6e.16xlarge", vec!["dedicatedHostsSupported"], "false"),
        ("g6e.24xlarge", vec!["dedicatedHostsSupported"], "true"),
        ("g6e.48xlarge", vec!["dedicatedHostsSupported"], "true"),
        // Nitro Enclaves and NitroTPM: neither is offered on the accelerated family, while the
        // graphics family is offered both on its virtualized sizes.
        ("trn1.2xlarge", vec!["nitroEnclavesSupport"], "unsupported"),
        ("trn1.32xlarge", vec!["nitroEnclavesSupport"], "unsupported"),
        ("g6e.xlarge", vec!["nitroEnclavesSupport"], "supported"),
        ("trn1.32xlarge", vec!["nitroTpmSupport"], "unsupported"),
        ("g6e.xlarge", vec!["nitroTpmSupport"], "supported"),
        // Reboot migration and the local PTP hardware clock: no size of these three families is
        // offered either.
        ("g6e.12xlarge", vec!["rebootMigrationSupport"], "unsupported"),
        ("r5dn.12xlarge", vec!["rebootMigrationSupport"], "unsupported"),
        ("trn1.2xlarge", vec!["rebootMigrationSupport"], "unsupported"),
        ("g6e.12xlarge", vec!["phcSupport"], "unsupported"),
        ("r5dn.metal", vec!["phcSupport"], "unsupported"),
        ("trn1.32xlarge", vec!["phcSupport"], "unsupported"),
        // The vCPUs of none of them run on CPU credits.
        ("g6e.xlarge", vec!["cpuBurstModel"], "none"),
        ("r5dn.large", vec!["cpuBurstModel"], "none"),
        ("trn1.2xlarge", vec!["cpuBurstModel"], "none"),
        // The packets of a session are copied under the guest of a virtualized size, and not
        // under a bare metal one of this hardware generation.
        (
            "r5dn.metal",
            vec!["networkInfo", "trafficMirroringSupport"],
            "unsupported",
        ),
        (
            "g6e.xlarge",
            vec!["networkInfo", "trafficMirroringSupport"],
            "supported",
        ),
    ];

    for (instance_type, path, expected) in rows {
        let case = format!("{instance_type} {path:?}");
        let body = described(&case, &router, instance_type).await;
        let reported = required(&case, &body, &path);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {path:?} {expected:?}, got {reported:?}"
        );
    }

    // The lists: the boot modes these three families are offered — the graphics family both of
    // them, the two older hardware generations the legacy BIOS alone — and the usage classes a
    // Capacity Block reserves a whole accelerated host for.
    for (instance_type, path, expected) in [
        (
            "g6e.xlarge",
            vec!["supportedBootModes"],
            vec!["legacy-bios", "uefi"],
        ),
        ("r5dn.metal", vec!["supportedBootModes"], vec!["legacy-bios"]),
        (
            "trn1.32xlarge",
            vec!["supportedBootModes"],
            vec!["legacy-bios"],
        ),
        (
            "trn1.2xlarge",
            vec!["supportedUsageClasses"],
            vec!["on-demand", "spot"],
        ),
        (
            "trn1.32xlarge",
            vec!["supportedUsageClasses"],
            vec!["capacity-block", "on-demand", "spot"],
        ),
    ] {
        let case = format!("{instance_type} {path:?}");
        let body = described(&case, &router, instance_type).await;
        let reported = reported_instance_type_list(&case, &body, &path);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {path:?} {expected:?}, got {reported:?}"
        );
    }

    // The ONE network card of a narrow size of the memory family carries the whole published
    // figure of that size — the bandwidth it sustains, the ceiling it bursts to, the text of
    // those two figures and the interfaces that attach to it — so the card and the instance
    // cannot report different networks.
    let case = "r5dn.large networkCards";
    let body = described(case, &router, "r5dn.large").await;
    for (member, expected) in [
        ("networkCardIndex", "0"),
        ("baselineBandwidthInGbps", "2.1"),
        ("peakBandwidthInGbps", "25.0"),
        ("maximumNetworkInterfaces", "3"),
        ("networkPerformance", "Up to 25 Gigabit"),
    ] {
        let reported = required(case, &body, &["networkInfo", "networkCards", "item", member]);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the card's {member} {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, the sharp edge: a published row is NOT a function of the vCPUs and memory the name
/// gives. Two families at ONE size disagree on every figure, two families on ONE host agree on
/// the figures that host publishes and differ on the hardware the capability letter adds, and
/// the core counts a caller may configure are the published list rather than every count up to
/// the default.
#[tokio::test]
async fn the_published_rows_are_not_a_function_of_the_size() {
    let (router, _engine) = emulator();

    // One size, two graphics families: the same 4 vCPUs, and a different memory ratio, a
    // different accelerator and a different network column.
    let plain = described("g6.xlarge", &router, "g6.xlarge").await;
    let extra = described("g6e.xlarge", &router, "g6e.xlarge").await;
    for (case, body) in [("g6.xlarge", &plain), ("g6e.xlarge", &extra)] {
        let vcpus = number(case, body, &["vCpuInfo", "defaultVCpus"]) as i64;
        assert_eq!(
            vcpus, 4,
            "case {case:?}: both rows must be the same size, got {vcpus} vCPUs"
        );
    }
    for (member, left, right) in [
        ("memoryInfo.sizeInMiB", 16_384_i64, 32_768_i64),
        ("gpuInfo.totalGpuMemoryInMiB", 22_888, 45_776),
    ] {
        let path: Vec<&str> = member.split('.').collect();
        let reported = number("g6.xlarge", &plain, &path) as i64;
        assert_eq!(
            reported, left,
            "case \"g6.xlarge\": expected {member} {left}, got {reported}"
        );
        let reported = number("g6e.xlarge", &extra, &path) as i64;
        assert_eq!(
            reported, right,
            "case \"g6e.xlarge\": expected {member} {right}, got {reported}"
        );
    }
    assert_ne!(
        required("g6.xlarge", &plain, &["networkInfo", "networkPerformance"]),
        required("g6e.xlarge", &extra, &["networkInfo", "networkPerformance"]),
        "two graphics families of one size publish different network performance"
    );

    // One host, two memory families: the EBS figures and the local devices of the host are the
    // same for both, while the network the capability letter adds is not — which no rule that
    // derives one from the other produces.
    let plain = described("r5d.12xlarge", &router, "r5d.12xlarge").await;
    let networked = described("r5dn.12xlarge", &router, "r5dn.12xlarge").await;
    for member in [
        "baselineBandwidthInMbps",
        "baselineIops",
        "maximumBandwidthInMbps",
        "maximumIops",
    ] {
        let path = vec!["ebsInfo", "ebsOptimizedInfo", member];
        let left = required("r5d.12xlarge", &plain, &path);
        let right = required("r5dn.12xlarge", &networked, &path);
        assert_eq!(
            left, right,
            "case \"r5dn.12xlarge\": the two families on one host publish the same {member}, \
             got {left:?} and {right:?}"
        );
    }
    let path = vec!["instanceStorageInfo", "totalSizeInGB"];
    assert_eq!(
        required("r5d.12xlarge", &plain, &path),
        required("r5dn.12xlarge", &networked, &path),
        "the two families on one host publish the same instance store"
    );
    let path = vec!["networkInfo", "networkPerformance"];
    assert_ne!(
        required("r5d.12xlarge", &plain, &path),
        required("r5dn.12xlarge", &networked, &path),
        "the network-optimized family publishes a network of its own"
    );

    // The core counts a caller may configure: every count up to the default on the sizes whose
    // cores sit on one processor socket, the even counts alone on the sizes that span both, the
    // default alone on a size that takes its cores all at once, and every count again on the
    // bare metal size that IS the host.
    for (instance_type, expected) in [
        ("g6e.4xlarge", (1..=8).collect::<Vec<i64>>()),
        ("g6e.8xlarge", (1..=16).filter(|core| core % 2 == 0).collect()),
        ("g6e.12xlarge", vec![24]),
        ("g6e.48xlarge", vec![96]),
        ("r5dn.xlarge", vec![1, 2]),
        ("r5dn.2xlarge", vec![2, 4]),
        ("r5dn.24xlarge", vec![48]),
        ("r5dn.metal", (1..=48).collect()),
        ("trn1.2xlarge", vec![2, 4]),
        (
            "trn1.32xlarge",
            (1..=64).filter(|core| core % 2 == 0).collect(),
        ),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported: Vec<i64> = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"])
            .iter()
            .map(|core| {
                core.parse()
                    .unwrap_or_else(|_| panic!("case {case:?}: a core count, got {core:?}"))
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected validCores {expected:?}, got {reported:?}"
        );
    }
}
