// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for the graphics families — the x86 one
//! and the AWS Graviton one — the fourth generation general purpose and memory optimized
//! families on the Xen hypervisor, and the memory optimized family with local NVMe devices,
//! driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: EVERY MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE TYPE IS
//! THE FACT AWS PUBLISHES FOR THAT ONE TYPE, read off a per-type table — never a function of
//! the family letter and the size multiplier in its name. So a family can publish memory that
//! is half its family letter's ratio or not a whole number of GiB, local devices whose count
//! and size are the size's own (or none at all where its family letter reads as having them),
//! accelerators only the type's own row knows about, an EBS attachment limit below what its
//! other devices alone would leave, a `shared` limit on pre-Nitro hardware, an adapter that
//! answers `unsupported` on every size but the largest, a network column published as WORDS,
//! a flag that is false on a family whose generation would answer true, and core counts that
//! are not every count up to the default — none of which any function of the size produces.
//!
//! The tables below span what the rule generalises over: five families differing in hypervisor
//! (Xen and Nitro), in processor (Intel, AMD and AWS Graviton), in having accelerators, local
//! devices or neither, and in whether they are the latest generation of their family; inside
//! each family the smallest, several middle, the largest virtualized and (where the family has
//! one) the bare metal size are rows, so no one size reads as the specification. The figures
//! are the ones AWS states per type: each family's own specification page (vCPUs, memory,
//! accelerators, local devices, network performance), the "Amazon EBS optimized instance
//! types" table (the EBS figures), "Instance volume limits" (the attachment limit and its
//! kind), the "IP addresses per network interface per instance type" table (the interface and
//! address limits) and the published lists of the types offered enhanced networking with ENA,
//! an Elastic Fabric Adapter and Dedicated Hosts.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    described_instance_type, emulator, reported_instance_type_list, reported_instance_type_member,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The megabits in a megabyte, which is all that separates the `Mbps` figures of
/// `EbsOptimizedInfo` from its `MB/s` ones.
const MEGABITS_PER_MEGABYTE: f64 = 8.0;

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

/// One row of the accelerator table: an instance type, and its accelerators as `(count, the
/// device's name, the MiB of one device)` where it carries any at all.
type AcceleratorRow = (&'static str, Option<(i64, &'static str, i64)>);

/// The published hardware row of one instance type.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `g5.8xlarge` and the like.
    instance_type: &'static str,
    /// Its memory, in MiB.
    memory_mib: i64,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// The EBS volumes it holds attached, and whether that limit is shared with its interfaces
    /// and local devices or is the volumes' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none at all.
    instance_store: Option<(i64, i64)>,
    /// Its network performance, as the published table writes that column.
    network_performance: &'static str,
    /// Its processor's maker and that processor's sustained frequency, in GHz.
    processor: (&'static str, f64),
}

/// The rows of the five families, spanning their ladders.
///
/// The x86 graphics family publishes a dedicated EBS figure from its `8xlarge` up while the
/// Graviton one publishes a different pair at the same size; the Graviton one carries NO local
/// devices at any size where its family letter reads as having them; the two Xen families
/// publish ONE fixed EBS figure per size, a `shared` attachment limit well above the Nitro
/// pool, and a network column in words on the general purpose one; and the NVMe memory
/// optimized family's device count grows in steps of its own while the size of one device
/// changes with it.
const ROWS: &[Row] = &[
    Row {
        instance_type: "g5.xlarge",
        memory_mib: 16_384,
        ebs: (700, 3_000, 3_500, 15_000),
        ebs_attachments: (25, "shared"),
        instance_store: Some((1, 250)),
        network_performance: "Up to 10 Gigabit",
        processor: ("AMD", 3.3),
    },
    Row {
        instance_type: "g5.8xlarge",
        memory_mib: 131_072,
        ebs: (16_000, 65_000, 16_000, 65_000),
        ebs_attachments: (25, "shared"),
        instance_store: Some((1, 900)),
        network_performance: "25 Gigabit",
        processor: ("AMD", 3.3),
    },
    Row {
        instance_type: "g5.12xlarge",
        memory_mib: 196_608,
        ebs: (16_000, 65_000, 16_000, 65_000),
        // Four accelerators draw on the same pool the volumes come out of, so this size holds
        // three fewer volumes than the one above it with the same local device.
        ebs_attachments: (22, "shared"),
        instance_store: Some((1, 3_800)),
        network_performance: "40 Gigabit",
        processor: ("AMD", 3.3),
    },
    Row {
        instance_type: "g5.16xlarge",
        memory_mib: 262_144,
        ebs: (16_000, 65_000, 16_000, 65_000),
        ebs_attachments: (25, "shared"),
        instance_store: Some((1, 1_900)),
        network_performance: "25 Gigabit",
        processor: ("AMD", 3.3),
    },
    Row {
        instance_type: "g5.24xlarge",
        memory_mib: 393_216,
        ebs: (19_000, 80_000, 19_000, 80_000),
        ebs_attachments: (22, "shared"),
        instance_store: Some((1, 3_800)),
        network_performance: "50 Gigabit",
        processor: ("AMD", 3.3),
    },
    Row {
        instance_type: "g5g.xlarge",
        // HALF the system memory per vCPU its family letter's ratio would give.
        memory_mib: 8_192,
        ebs: (1_188, 6_000, 4_750, 20_000),
        ebs_attachments: (26, "shared"),
        instance_store: None,
        network_performance: "Up to 10 Gigabit",
        processor: ("AWS", 2.5),
    },
    Row {
        instance_type: "g5g.2xlarge",
        memory_mib: 16_384,
        ebs: (2_375, 12_000, 4_750, 20_000),
        ebs_attachments: (26, "shared"),
        instance_store: None,
        network_performance: "Up to 10 Gigabit",
        processor: ("AWS", 2.5),
    },
    Row {
        instance_type: "g5g.8xlarge",
        memory_mib: 65_536,
        ebs: (9_500, 40_000, 9_500, 40_000),
        ebs_attachments: (26, "shared"),
        instance_store: None,
        network_performance: "12 Gigabit",
        processor: ("AWS", 2.5),
    },
    Row {
        instance_type: "g5g.16xlarge",
        memory_mib: 131_072,
        ebs: (19_000, 80_000, 19_000, 80_000),
        ebs_attachments: (25, "shared"),
        instance_store: None,
        network_performance: "25 Gigabit",
        processor: ("AWS", 2.5),
    },
    Row {
        instance_type: "m4.large",
        memory_mib: 8_192,
        // A pre-Nitro instance has ONE fixed figure rather than a baseline it bursts above.
        ebs: (450, 3_600, 450, 3_600),
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: "Moderate",
        processor: ("Intel", 2.4),
    },
    Row {
        instance_type: "m4.2xlarge",
        memory_mib: 32_768,
        ebs: (1_000, 8_000, 1_000, 8_000),
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: "High",
        processor: ("Intel", 2.4),
    },
    Row {
        instance_type: "m4.10xlarge",
        memory_mib: 163_840,
        ebs: (4_000, 32_000, 4_000, 32_000),
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: "10 Gigabit",
        processor: ("Intel", 2.4),
    },
    Row {
        instance_type: "m4.16xlarge",
        memory_mib: 262_144,
        ebs: (10_000, 65_000, 10_000, 65_000),
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: "25 Gigabit",
        // The size that holds the whole host is built on a different processor from the sizes
        // below it, so even one family's frequency is not one figure.
        processor: ("Intel", 2.3),
    },
    Row {
        instance_type: "r4.large",
        // 15.25 GiB, which is not a whole number of GiB at any size of the family.
        memory_mib: 15_616,
        ebs: (425, 3_000, 425, 3_000),
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: "Up to 10 Gigabit",
        processor: ("Intel", 2.3),
    },
    Row {
        instance_type: "r4.2xlarge",
        memory_mib: 62_464,
        ebs: (1_700, 12_000, 1_700, 12_000),
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: "Up to 10 Gigabit",
        processor: ("Intel", 2.3),
    },
    Row {
        instance_type: "r4.16xlarge",
        memory_mib: 499_712,
        ebs: (14_000, 75_000, 14_000, 75_000),
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: "25 Gigabit",
        processor: ("Intel", 2.3),
    },
    Row {
        instance_type: "r5d.large",
        memory_mib: 16_384,
        ebs: (650, 3_600, 4_750, 18_750),
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 75)),
        network_performance: "Up to 10 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5d.4xlarge",
        memory_mib: 131_072,
        ebs: (4_750, 18_750, 4_750, 18_750),
        // Two local devices leave one volume fewer than the one device of the sizes below.
        ebs_attachments: (25, "shared"),
        instance_store: Some((2, 300)),
        network_performance: "Up to 10 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5d.8xlarge",
        memory_mib: 262_144,
        ebs: (6_800, 30_000, 6_800, 30_000),
        ebs_attachments: (25, "shared"),
        // The same total as the `12xlarge` below, in devices of a different size.
        instance_store: Some((2, 600)),
        network_performance: "10 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5d.12xlarge",
        memory_mib: 393_216,
        ebs: (9_500, 40_000, 9_500, 40_000),
        ebs_attachments: (25, "shared"),
        instance_store: Some((2, 900)),
        network_performance: "12 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5d.16xlarge",
        memory_mib: 524_288,
        ebs: (13_600, 60_000, 13_600, 60_000),
        ebs_attachments: (23, "shared"),
        instance_store: Some((4, 600)),
        network_performance: "20 Gigabit",
        processor: ("Intel", 3.1),
    },
    Row {
        instance_type: "r5d.metal",
        memory_mib: 786_432,
        ebs: (19_000, 80_000, 19_000, 80_000),
        // A bare metal instance's local devices are the guest's own hardware, so the volumes
        // it holds are not reduced by them.
        ebs_attachments: (31, "shared"),
        instance_store: Some((4, 900)),
        network_performance: "25 Gigabit",
        processor: ("Intel", 3.1),
    },
];

/// Rule: the memory, EBS, attachment, instance store, network and processor figures of these
/// five families are the ones AWS publishes for each type.
#[tokio::test]
async fn hardware_rows_of_the_graphics_and_fourth_generation_families_are_published_facts() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let reported = number(case, &body, &["memoryInfo", "sizeInMiB"]) as i64;
        assert_eq!(
            reported, row.memory_mib,
            "case {case:?}: expected memoryInfo.sizeInMiB {}, got {reported}",
            row.memory_mib
        );

        let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = row.ebs;
        for (member, expected) in [
            ("baselineBandwidthInMbps", baseline_mbps as f64),
            ("baselineIops", baseline_iops as f64),
            ("maximumBandwidthInMbps", maximum_mbps as f64),
            ("maximumIops", maximum_iops as f64),
            (
                "baselineThroughputInMBps",
                baseline_mbps as f64 / MEGABITS_PER_MEGABYTE,
            ),
            (
                "maximumThroughputInMBps",
                maximum_mbps as f64 / MEGABITS_PER_MEGABYTE,
            ),
        ] {
            let reported = number(case, &body, &["ebsInfo", "ebsOptimizedInfo", member]);
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected {member} {expected}, got {reported}"
            );
        }

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

        let supported = required(case, &body, &["instanceStorageSupported"]);
        let total = reported_instance_type_member(case, &body, &["instanceStorageInfo", "totalSizeInGB"]);
        match row.instance_store {
            Some((devices, gb_each)) => {
                assert_eq!(
                    supported, "true",
                    "case {case:?}: a type with local devices reports instanceStorageSupported \
                     true, got {supported:?}"
                );
                let reported =
                    number(case, &body, &["instanceStorageInfo", "disks", "item", "count"]) as i64;
                assert_eq!(
                    reported, devices,
                    "case {case:?}: expected {devices} local devices, got {reported}"
                );
                let reported =
                    number(case, &body, &["instanceStorageInfo", "disks", "item", "sizeInGB"])
                        as i64;
                assert_eq!(
                    reported, gb_each,
                    "case {case:?}: expected local devices of {gb_each} GB, got {reported}"
                );
                let expected = devices * gb_each;
                let reported = total
                    .unwrap_or_else(|| panic!("case {case:?}: no totalSizeInGB, got {body}"))
                    .parse::<i64>()
                    .unwrap_or_else(|_| panic!("case {case:?}: a total, got {body}"));
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected totalSizeInGB {expected}, got {reported}"
                );
            }
            None => {
                assert_eq!(
                    supported, "false",
                    "case {case:?}: a type with no local devices reports \
                     instanceStorageSupported false, got {supported:?}"
                );
                assert!(
                    total.is_none(),
                    "case {case:?}: a type with no local devices describes no instance store, \
                     got {total:?}"
                );
            }
        }

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

/// Rule: the accelerators of an instance type are published per type — how many devices, which
/// device, the memory of one and the total — and a type that carries none describes none.
#[tokio::test]
async fn accelerator_inventories_are_published_per_type() {
    let (router, _engine) = emulator();

    // Rows: one device on the small x86 graphics sizes, four and eight on the sizes that hold
    // several, one and two on the Graviton family's two halves of its ladder, and the three
    // families with no accelerator at all — including the bare metal size of one of them.
    let rows: &[AcceleratorRow] = &[
        ("g5.xlarge", Some((1, "A10G", 22_888))),
        ("g5.8xlarge", Some((1, "A10G", 22_888))),
        ("g5.12xlarge", Some((4, "A10G", 22_888))),
        ("g5.48xlarge", Some((8, "A10G", 22_888))),
        ("g5g.xlarge", Some((1, "T4g", 16_384))),
        ("g5g.16xlarge", Some((2, "T4g", 16_384))),
        ("g5g.metal", Some((2, "T4g", 16_384))),
        ("m4.4xlarge", None),
        ("r4.8xlarge", None),
        ("r5d.2xlarge", None),
        ("r5d.metal", None),
    ];

    for (instance_type, accelerators) in rows {
        let case = *instance_type;
        let body = described(case, &router, instance_type).await;
        let total = reported_instance_type_member(case, &body, &["gpuInfo", "totalGpuMemoryInMiB"]);
        match accelerators {
            Some((count, name, memory_mib_each)) => {
                let reported =
                    number(case, &body, &["gpuInfo", "gpus", "item", "count"]) as i64;
                assert_eq!(
                    reported, *count,
                    "case {case:?}: expected {count} accelerators, got {reported}"
                );
                let reported = required(case, &body, &["gpuInfo", "gpus", "item", "name"]);
                assert_eq!(
                    reported, *name,
                    "case {case:?}: expected the accelerator {name:?}, got {reported:?}"
                );
                let reported = required(case, &body, &["gpuInfo", "gpus", "item", "manufacturer"]);
                assert_eq!(
                    reported, "NVIDIA",
                    "case {case:?}: expected the accelerator's maker, got {reported:?}"
                );
                let reported = number(
                    case,
                    &body,
                    &["gpuInfo", "gpus", "item", "memoryInfo", "sizeInMiB"],
                ) as i64;
                assert_eq!(
                    reported, *memory_mib_each,
                    "case {case:?}: expected {memory_mib_each} MiB per accelerator, got {reported}"
                );
                // The total is one device's memory times the count, so the two readings of one
                // inventory cannot disagree.
                let expected = count * memory_mib_each;
                let reported = total
                    .unwrap_or_else(|| panic!("case {case:?}: no total accelerator memory"))
                    .parse::<i64>()
                    .unwrap_or_else(|_| panic!("case {case:?}: a total, got {body}"));
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected totalGpuMemoryInMiB {expected}, got {reported}"
                );
            }
            None => assert!(
                total.is_none(),
                "case {case:?}: a type with no accelerator describes none, got {total:?}"
            ),
        }
    }
}

/// Rule: the capabilities and host features a type is offered with are published per type, so
/// neither the family nor the hardware generation answers for every size on it.
#[tokio::test]
async fn capabilities_and_host_features_are_published_per_type() {
    let (router, _engine) = emulator();

    // Rows: for each member, one type that has the capability beside one that does not, taken
    // from both halves of each family's ladder.
    let network_rows: &[(&str, &str, &str)] = &[
        // The Elastic Fabric Adapter is offered from the x86 graphics size that holds a
        // dedicated share of the host's network up, and on no size of the Graviton family.
        ("g5.xlarge", "efaSupported", "false"),
        ("g5.4xlarge", "efaSupported", "false"),
        ("g5.8xlarge", "efaSupported", "true"),
        ("g5.48xlarge", "efaSupported", "true"),
        ("g5g.16xlarge", "efaSupported", "false"),
        ("r5d.24xlarge", "efaSupported", "false"),
        // The x86 graphics family's adapter encrypts traffic between instances; the fourth
        // generation families and the fifth generation memory optimized one do not.
        ("g5.xlarge", "encryptionInTransitSupported", "true"),
        ("g5.24xlarge", "encryptionInTransitSupported", "true"),
        ("g5g.4xlarge", "encryptionInTransitSupported", "false"),
        ("m4.10xlarge", "encryptionInTransitSupported", "false"),
        ("r5d.8xlarge", "encryptionInTransitSupported", "false"),
        // Enhanced networking with ENA reaches the general purpose Xen family's largest size
        // alone, while the memory optimized Xen family carries it at every size and a Nitro
        // type has no other network device at all.
        ("m4.large", "enaSupport", "unsupported"),
        ("m4.10xlarge", "enaSupport", "unsupported"),
        ("m4.16xlarge", "enaSupport", "supported"),
        ("r4.large", "enaSupport", "supported"),
        ("r4.16xlarge", "enaSupport", "supported"),
        ("r5d.large", "enaSupport", "required"),
        ("g5.12xlarge", "enaSupport", "required"),
        // IPv6 reaches these families whether or not they are the latest generation of theirs.
        ("g5.12xlarge", "ipv6Supported", "true"),
        ("m4.16xlarge", "ipv6Supported", "true"),
        ("r4.xlarge", "ipv6Supported", "true"),
        // The packets of a session are copied under the guest on a virtualized size, and by
        // the host's own card on a bare metal one.
        ("g5g.metal", "trafficMirroringSupport", "supported"),
        ("r5d.metal", "trafficMirroringSupport", "supported"),
        ("r5d.2xlarge", "trafficMirroringSupport", "supported"),
    ];

    for (instance_type, member, expected) in network_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let reported = required(&case, &body, &["networkInfo", *member]);
        assert_eq!(
            reported, *expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The interface and address limits: the two accelerated sizes that hold several devices
    // take them from the host's share rather than from their step of the size ladder, and the
    // Xen family's smallest and largest sizes publish limits of their own.
    let limit_rows: &[(&str, i64, i64)] = &[
        ("g5.2xlarge", 4, 15),
        ("g5.4xlarge", 8, 30),
        ("g5.12xlarge", 15, 50),
        ("g5.16xlarge", 8, 30),
        ("g5.24xlarge", 15, 50),
        ("g5.48xlarge", 7, 50),
        ("m4.large", 2, 10),
        ("m4.4xlarge", 8, 30),
        ("m4.16xlarge", 8, 30),
        ("r4.16xlarge", 15, 50),
    ];

    for (instance_type, interfaces, addresses) in limit_rows {
        let case = *instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = number(case, &body, &["networkInfo", "maximumNetworkInterfaces"]) as i64;
        assert_eq!(
            reported, *interfaces,
            "case {case:?}: expected maximumNetworkInterfaces {interfaces}, got {reported}"
        );
        for member in ["ipv4AddressesPerInterface", "ipv6AddressesPerInterface"] {
            let reported = number(case, &body, &["networkInfo", member]) as i64;
            assert_eq!(
                reported, *addresses,
                "case {case:?}: expected {member} {addresses}, got {reported}"
            );
        }
    }

    // The host features, each a flag a client branches on.
    let feature_rows: &[(&str, &str, &str)] = &[
        // Neither graphics family nor the fourth generation general purpose one is the latest
        // generation of its family; the Graviton graphics family and the NVMe memory optimized
        // one are.
        ("g5.xlarge", "currentGeneration", "false"),
        ("g5.48xlarge", "currentGeneration", "false"),
        ("m4.large", "currentGeneration", "false"),
        ("r4.16xlarge", "currentGeneration", "false"),
        ("g5g.4xlarge", "currentGeneration", "true"),
        ("r5d.12xlarge", "currentGeneration", "true"),
        // The Dedicated Host carries the whole host's accelerators, so only the accelerated
        // sizes that hold several of them are offered one — while the general purpose Xen
        // family is offered one at every size BUT the one that is the whole host.
        ("g5.xlarge", "dedicatedHostsSupported", "false"),
        ("g5.16xlarge", "dedicatedHostsSupported", "false"),
        ("g5.24xlarge", "dedicatedHostsSupported", "true"),
        ("g5.48xlarge", "dedicatedHostsSupported", "true"),
        ("g5g.2xlarge", "dedicatedHostsSupported", "false"),
        ("g5g.16xlarge", "dedicatedHostsSupported", "true"),
        ("m4.4xlarge", "dedicatedHostsSupported", "true"),
        ("m4.16xlarge", "dedicatedHostsSupported", "false"),
        ("r4.8xlarge", "dedicatedHostsSupported", "true"),
        // Action based recovery cannot carry the data of a local instance store over, so the
        // family with no local devices is offered it at every virtualized size and the two
        // with them at none.
        ("g5g.4xlarge", "autoRecoverySupported", "true"),
        ("g5g.16xlarge", "autoRecoverySupported", "true"),
        ("g5g.metal", "autoRecoverySupported", "false"),
        ("g5.8xlarge", "autoRecoverySupported", "false"),
        ("r5d.large", "autoRecoverySupported", "false"),
        ("m4.2xlarge", "autoRecoverySupported", "true"),
        // Reboot migration is offered on the Xen families and the Graviton graphics one, and
        // on no size of the x86 graphics family or the NVMe memory optimized one.
        ("m4.large", "rebootMigrationSupport", "supported"),
        ("r4.2xlarge", "rebootMigrationSupport", "supported"),
        ("g5g.8xlarge", "rebootMigrationSupport", "supported"),
        ("g5.12xlarge", "rebootMigrationSupport", "unsupported"),
        ("r5d.8xlarge", "rebootMigrationSupport", "unsupported"),
        ("r5d.metal", "rebootMigrationSupport", "unsupported"),
        // No hardware generation here carries a local clock.
        ("g5.24xlarge", "phcSupport", "unsupported"),
        ("m4.16xlarge", "phcSupport", "unsupported"),
        ("r5d.24xlarge", "phcSupport", "unsupported"),
        // None of these families runs its vCPUs at a baseline it spends credits to burst
        // above, so none has a burst model.
        ("g5.4xlarge", "cpuBurstModel", "none"),
        ("m4.xlarge", "cpuBurstModel", "none"),
        ("r5d.xlarge", "cpuBurstModel", "none"),
        // A bare metal instance has no hypervisor to present a trusted platform module.
        ("r5d.metal", "nitroTpmSupport", "unsupported"),
        ("g5g.metal", "nitroTpmSupport", "unsupported"),
        ("r5d.24xlarge", "nitroTpmSupport", "supported"),
    ];

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let reported = required(&case, &body, &[*member]);
        assert_eq!(
            reported, *expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, the sharp edge: the published core counts are not every count up to the type's
/// default, and two families at ONE size disagree on the figures the size alone would give.
#[tokio::test]
async fn the_published_rows_are_not_a_function_of_the_size() {
    let (router, _engine) = emulator();

    // The core counts a caller may configure: the NVMe memory optimized family takes its cores
    // two at a time from the `xlarge` to its largest virtualized size, which takes them all at
    // once, while the size that holds the whole host publishes every count; the Xen families
    // publish every count up to the size whose cores sit on two sockets.
    let core_rows: &[(&str, Vec<i64>)] = &[
        ("r5d.large", vec![1]),
        ("r5d.xlarge", vec![2]),
        ("r5d.2xlarge", vec![2, 4]),
        ("r5d.4xlarge", vec![2, 4, 6, 8]),
        ("r5d.24xlarge", vec![48]),
        ("r5d.metal", (1..=48).collect()),
        ("m4.4xlarge", (1..=8).collect()),
        ("m4.10xlarge", (1..=10).map(|count| count * 2).collect()),
        ("r4.8xlarge", (1..=16).collect()),
        ("r4.16xlarge", (1..=16).map(|count| count * 2).collect()),
        ("g5.4xlarge", (1..=8).collect()),
    ];

    for (instance_type, expected) in core_rows {
        let case = *instance_type;
        let body = described(case, &router, instance_type).await;
        let reported: Vec<i64> = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"])
            .iter()
            .map(|count| {
                count
                    .parse()
                    .unwrap_or_else(|_| panic!("case {case:?}: a core count, got {count:?}"))
            })
            .collect();
        assert_eq!(
            &reported, expected,
            "case {case:?}: expected validCores {expected:?}, got {reported:?}"
        );
    }

    // Two families at ONE size — 64 vCPUs each — disagreeing on the EBS figures, on the
    // network column and on how many volumes they hold, which no function of the size gives.
    let left = described("r4.16xlarge", &router, "r4.16xlarge").await;
    let right = described("r5d.16xlarge", &router, "r5d.16xlarge").await;
    for (case, body) in [("r4.16xlarge", &left), ("r5d.16xlarge", &right)] {
        let vcpus = number(case, body, &["vCpuInfo", "defaultVCpus"]) as i64;
        assert_eq!(
            vcpus, 64,
            "case {case:?}: both rows must be the same size, got {vcpus} vCPUs"
        );
    }
    for member in [
        vec!["ebsInfo", "ebsOptimizedInfo", "baselineBandwidthInMbps"],
        vec!["ebsInfo", "maximumEbsAttachments"],
        vec!["networkInfo", "networkPerformance"],
    ] {
        let one = required("r4.16xlarge", &left, &member);
        let other = required("r5d.16xlarge", &right, &member);
        assert_ne!(
            one, other,
            "two families of one size publish different {member:?}, got {one:?} for both"
        );
    }

    // And one family's largest virtualized size sustains what it reaches while its smallest
    // stays below its own ceiling, which no "the figure grows with the size" rule produces on
    // its own.
    for (instance_type, bursts) in [
        ("r5d.large", true),
        ("r5d.2xlarge", true),
        ("r5d.8xlarge", false),
        ("r5d.24xlarge", false),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let baseline = number(
            case,
            &body,
            &["ebsInfo", "ebsOptimizedInfo", "baselineBandwidthInMbps"],
        );
        let maximum = number(
            case,
            &body,
            &["ebsInfo", "ebsOptimizedInfo", "maximumBandwidthInMbps"],
        );
        assert_eq!(
            baseline < maximum,
            bursts,
            "case {case:?}: expected bursting {bursts}, got {baseline}/{maximum}"
        );
    }
}
