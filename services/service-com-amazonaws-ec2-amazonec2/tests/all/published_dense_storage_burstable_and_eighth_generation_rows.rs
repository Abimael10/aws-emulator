// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for the dense HDD storage families, the
//! burstable family on pre-Nitro hardware, the eighth generation Intel families and the
//! general purpose and memory optimized families with local NVMe devices — driven over the
//! wire the service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered
//! with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: EVERY MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE TYPE IS
//! THE FIGURE AWS PUBLISHES FOR THAT ONE TYPE, not a function of the size multiplier in its
//! name. The four EBS figures (`EbsInfo$EbsOptimizedInfo`, whose baseline and maximum are
//! independent members), whether the type has an EBS-optimized mode at all
//! (`$EbsOptimizedSupport`), how many volumes it holds attached and whether that limit is its
//! own or shared with its other devices (`$MaximumEbsAttachments` / `$AttachmentLimitType`),
//! its local devices (`InstanceStorageInfo`), its network performance, its adapter's ENA
//! support, its Elastic Fabric Adapters, its connection-tracking timeouts, its processor and
//! that processor's frequency, its vCPUs' cores and threads, whether it is the latest
//! generation of its family, whether it is offered on a Dedicated Host and which root device
//! types and architectures its AMIs may use are each a published fact of the type.
//!
//! The tables below span what the rule generalises over: six families differing in hypervisor
//! (Xen and Nitro), in generation (a 2015 family through a 2025 one), in processor (Intel and
//! AMD), in having local devices at all and in whether their EBS limit is shared or dedicated;
//! inside each family several steps of the ladder are rows, so no one size is the
//! specification. The figures are the ones AWS states for each type: its family's
//! specification page (vCPUs, memory, instance store devices, network performance, the
//! processor's frequency), the "Amazon EBS optimized instance types" table (the EBS figures),
//! "Instance volume limits" (the attachment limit and its kind), "Connection tracking
//! timeouts", the "IP addresses per network interface per instance type" table and the
//! published lists of the types offered enhanced networking with ENA, an Elastic Fabric
//! Adapter, Dedicated Hosts and 32-bit AMIs.

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
    reported_instance_type_member(case, body, path).unwrap_or_else(|| {
        panic!("case {case:?}: no {path:?} reported, got {body}")
    })
}

/// One member of the reported type as a number.
fn number(case: &str, body: &str, path: &[&str]) -> f64 {
    let text = required(case, body, path);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {path:?} must be a number, got {text:?}"))
}

/// The published row of one instance type: the figures a client sizes its storage, its volume
/// attachments and its network from.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `d2.xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches), or `None` for a
    /// type with no EBS-optimized mode, which has no such performance to report.
    ebs: Option<(i64, i64, i64, i64)>,
    /// The EBS volumes it holds attached, and whether that limit is shared with its
    /// interfaces and local devices or is the volumes' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none at all.
    instance_store: Option<(i64, i64)>,
    /// Its network performance as the published table writes it, where its row publishes one.
    network_performance: Option<&'static str>,
}

/// Several steps of each family's ladder, with the figures AWS publishes for each.
///
/// T2: a current generation family on the Xen hypervisor — no EBS-optimized mode at all, and
/// 40 volumes shared with its other devices.
/// D2 and D3: dense HDD storage whose disks are 2,048 and 1,980 GB, where D2 publishes one
/// fixed EBS figure per size (a pre-Nitro type does not burst) and D3 a baseline that bursts
/// to the figure its largest size carries; D3's volumes share the Nitro attachment pool with
/// its two dozen disks, which is what leaves the `8xlarge` three of them.
/// M6id and R5ad: local NVMe devices published as a count and a size that follow no ratio.
/// C8i-flex and C8ib: one hardware generation whose EBS figures differ by a factor of five at
/// the same size, both with a volume limit of their own.
const ROWS: [Row; 15] = [
    Row {
        instance_type: "t2.nano",
        ebs: None,
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: Some("Low"),
    },
    Row {
        instance_type: "t2.small",
        ebs: None,
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: Some("Low to Moderate"),
    },
    Row {
        instance_type: "t2.2xlarge",
        ebs: None,
        ebs_attachments: (40, "shared"),
        instance_store: None,
        network_performance: Some("Moderate"),
    },
    Row {
        instance_type: "d2.xlarge",
        ebs: Some((750, 6_000, 750, 6_000)),
        ebs_attachments: (40, "shared"),
        instance_store: Some((3, 2_048)),
        network_performance: Some("Moderate"),
    },
    Row {
        instance_type: "d2.8xlarge",
        ebs: Some((4_000, 32_000, 4_000, 32_000)),
        ebs_attachments: (40, "shared"),
        instance_store: Some((24, 2_048)),
        network_performance: Some("10 Gigabit"),
    },
    Row {
        instance_type: "d3.xlarge",
        ebs: Some((850, 5_000, 5_000, 30_000)),
        ebs_attachments: (24, "shared"),
        instance_store: Some((3, 1_980)),
        network_performance: Some("Up to 15 Gigabit"),
    },
    Row {
        instance_type: "d3.8xlarge",
        ebs: Some((5_000, 30_000, 5_000, 30_000)),
        ebs_attachments: (3, "shared"),
        instance_store: Some((24, 1_980)),
        network_performance: Some("25 Gigabit"),
    },
    Row {
        instance_type: "m6id.large",
        ebs: Some((625, 3_600, 10_000, 40_000)),
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 118)),
        network_performance: Some("Up to 12.5 Gigabit"),
    },
    Row {
        instance_type: "m6id.12xlarge",
        ebs: Some((15_000, 60_000, 15_000, 60_000)),
        ebs_attachments: (25, "shared"),
        instance_store: Some((2, 1_425)),
        network_performance: Some("18.75 Gigabit"),
    },
    Row {
        instance_type: "m6id.32xlarge",
        ebs: Some((40_000, 160_000, 40_000, 160_000)),
        ebs_attachments: (23, "shared"),
        instance_store: Some((4, 1_900)),
        network_performance: Some("50 Gigabit"),
    },
    Row {
        instance_type: "r5ad.large",
        ebs: Some((650, 3_600, 2_880, 16_000)),
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 75)),
        network_performance: Some("Up to 10 Gigabit"),
    },
    Row {
        instance_type: "r5ad.24xlarge",
        ebs: Some((13_570, 60_000, 13_570, 60_000)),
        ebs_attachments: (23, "shared"),
        instance_store: Some((4, 900)),
        network_performance: Some("20 Gigabit"),
    },
    Row {
        instance_type: "c8i-flex.8xlarge",
        ebs: Some((5_000, 20_000, 10_000, 40_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        network_performance: None,
    },
    Row {
        instance_type: "c8i-flex.16xlarge",
        ebs: Some((10_000, 40_000, 20_000, 80_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        network_performance: None,
    },
    Row {
        instance_type: "c8ib.48xlarge",
        ebs: Some((150_000, 720_000, 150_000, 720_000)),
        ebs_attachments: (128, "dedicated"),
        instance_store: None,
        network_performance: None,
    },
];

/// Rule: the EBS, instance store and network members a type reports are the figures AWS
/// publishes for THAT type.
#[tokio::test]
async fn published_rows_report_the_hardware_facts_of_the_type() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        match row.ebs {
            Some((baseline_mbps, baseline_iops, maximum_mbps, maximum_iops)) => {
                for (member, expected) in [
                    ("baselineBandwidthInMbps", baseline_mbps),
                    ("baselineIops", baseline_iops),
                    ("maximumBandwidthInMbps", maximum_mbps),
                    ("maximumIops", maximum_iops),
                ] {
                    let path = ["ebsInfo", "ebsOptimizedInfo", member];
                    let reported = number(case, &body, &path) as i64;
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected {member} {expected}, got {reported}"
                    );
                }
                // The throughput members are the bandwidth ones in the other unit, which is
                // how the published table's Mbps and MB/s columns relate.
                for (member, bandwidth) in [
                    ("baselineThroughputInMBps", baseline_mbps),
                    ("maximumThroughputInMBps", maximum_mbps),
                ] {
                    let expected = bandwidth as f64 / MEGABITS_PER_MEGABYTE;
                    let reported = number(case, &body, &["ebsInfo", "ebsOptimizedInfo", member]);
                    assert!(
                        (reported - expected).abs() < 0.001,
                        "case {case:?}: expected {member} {expected}, got {reported}"
                    );
                }
            }
            None => {
                let reported =
                    reported_instance_type_member(case, &body, &["ebsInfo", "ebsOptimizedInfo"]);
                assert!(
                    reported.is_none(),
                    "case {case:?}: a type with no EBS-optimized mode reports no \
                     ebsOptimizedInfo, got {reported:?}"
                );
            }
        }

        let (attachments, limit_type) = row.ebs_attachments;
        let reported = number(case, &body, &["ebsInfo", "maximumEbsAttachments"]) as i64;
        assert_eq!(
            reported, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {reported}"
        );
        let reported = required(case, &body, &["ebsInfo", "attachmentLimitType"]);
        assert_eq!(
            reported, limit_type,
            "case {case:?}: expected attachmentLimitType {limit_type:?}, got {reported:?}"
        );

        match row.instance_store {
            Some((devices, gb_each)) => {
                let reported =
                    number(case, &body, &["instanceStorageInfo", "disks", "item", "count"]) as i64;
                assert_eq!(
                    reported, devices,
                    "case {case:?}: expected {devices} local devices, got {reported}"
                );
                let reported = number(
                    case,
                    &body,
                    &["instanceStorageInfo", "disks", "item", "sizeInGB"],
                ) as i64;
                assert_eq!(
                    reported, gb_each,
                    "case {case:?}: expected local devices of {gb_each} GB, got {reported}"
                );
                let total = devices * gb_each;
                let reported = number(case, &body, &["instanceStorageInfo", "totalSizeInGB"]) as i64;
                assert_eq!(
                    reported, total,
                    "case {case:?}: expected totalSizeInGB {total}, got {reported}"
                );
            }
            None => {
                let reported = reported_instance_type_member(case, &body, &["instanceStorageInfo"]);
                assert!(
                    reported.is_none(),
                    "case {case:?}: a type with no local devices reports no \
                     instanceStorageInfo, got {reported:?}"
                );
            }
        }

        if let Some(performance) = row.network_performance {
            let reported = required(case, &body, &["networkInfo", "networkPerformance"]);
            assert_eq!(
                reported, performance,
                "case {case:?}: expected networkPerformance {performance:?}, got {reported:?}"
            );
        }
    }
}

/// Rule, as an invariant over every row: a type sustains no more than it reaches, and the
/// figures do not follow the size multiplier — two families of one generation disagree at the
/// same size, and one family's `8xlarge` sustains less than another's `xlarge`.
#[tokio::test]
async fn the_published_figures_are_bounded_and_do_not_follow_the_size_multiplier() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let Some((baseline_mbps, baseline_iops, _, _)) = row.ebs else {
            continue;
        };
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let maximum_mbps =
            number(case, &body, &["ebsInfo", "ebsOptimizedInfo", "maximumBandwidthInMbps"]) as i64;
        let maximum_iops =
            number(case, &body, &["ebsInfo", "ebsOptimizedInfo", "maximumIops"]) as i64;
        assert!(
            baseline_mbps <= maximum_mbps && baseline_iops <= maximum_iops,
            "case {case:?}: a type sustains no more than it reaches, got baseline \
             {baseline_mbps} Mbps / {baseline_iops} IOPS against maximum {maximum_mbps} Mbps / \
             {maximum_iops} IOPS"
        );
    }

    // The block storage optimized family of the eighth generation sustains more at its
    // `xlarge` than the flexible family of the same generation does at its `8xlarge`, so no
    // function of the size multiplier can answer both.
    let flexible = "c8i-flex.8xlarge";
    let block_storage = "c8ib.xlarge";
    let flexible_body = described(flexible, &router, flexible).await;
    let block_storage_body = described(block_storage, &router, block_storage).await;
    let sustained = |case: &str, body: &str| -> i64 {
        number(case, body, &["ebsInfo", "ebsOptimizedInfo", "baselineBandwidthInMbps"]) as i64
    };
    let smaller = sustained(flexible, &flexible_body);
    let larger = sustained(block_storage, &block_storage_body);
    assert!(
        larger < smaller,
        "case {block_storage:?}: expected a sustained bandwidth below {flexible}'s {smaller} \
         Mbps, got {larger}"
    );
}

/// Rule: a published sustained bandwidth that is not a whole number of Mbps is reported as the
/// whole Mbps by the integer member and as the megabytes of the UNtruncated figure by the
/// throughput one, which is how the published table's two columns relate for such a size.
#[tokio::test]
async fn a_sustained_bandwidth_of_half_an_mbps_reports_both_readings() {
    let (router, _engine) = emulator();

    // Rows: the smallest size of the block storage optimized family, whose share of its
    // family's per-`xlarge` figure carries a half — 1,562.5 Mbps — beside a size of the same
    // family whose figure is whole, so the two readings are held apart.
    for (instance_type, whole_mbps, megabytes) in [
        ("c8ib.large", 1_562, 195.3125),
        ("c8ib.xlarge", 3_125, 390.625),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = number(
            case,
            &body,
            &["ebsInfo", "ebsOptimizedInfo", "baselineBandwidthInMbps"],
        ) as i64;
        assert_eq!(
            reported, whole_mbps,
            "case {case:?}: expected baselineBandwidthInMbps {whole_mbps}, got {reported}"
        );
        let reported = number(
            case,
            &body,
            &["ebsInfo", "ebsOptimizedInfo", "baselineThroughputInMBps"],
        );
        assert!(
            (reported - megabytes).abs() < 0.0001,
            "case {case:?}: expected baselineThroughputInMBps {megabytes}, got {reported}"
        );
    }
}

/// Rule: the capability facts a type reports — its generation, its EBS-optimized mode, its ENA
/// support, its burst model, its connection-tracking timeout, whether it is offered on a
/// Dedicated Host and its processor's frequency — are published per type too.
#[tokio::test]
async fn published_capability_facts_are_reported_per_type() {
    let (router, _engine) = emulator();

    // Rows: one member per case, each read on the type whose published answer is not the one
    // its family's generation number or hypervisor would give.
    let rows: [(&str, &[&str], &str); 25] = [
        // The dense HDD family AWS lists on its "Previous generation instances" page is not
        // the latest generation of its family, while its Nitro successor is.
        ("d2.xlarge", &["currentGeneration"], "false"),
        ("d2.8xlarge", &["currentGeneration"], "false"),
        ("d3.8xlarge", &["currentGeneration"], "true"),
        // EBS optimization is on by DEFAULT on the dense HDD family of 2015 and absent
        // altogether on the burstable Xen family.
        ("d2.xlarge", &["ebsInfo", "ebsOptimizedSupport"], "default"),
        ("t2.small", &["ebsInfo", "ebsOptimizedSupport"], "unsupported"),
        // Neither family is on the published list of the types offered enhanced networking
        // with ENA, while a Nitro type has no other network device.
        ("t2.small", &["networkInfo", "enaSupport"], "unsupported"),
        ("t2.2xlarge", &["networkInfo", "enaSupport"], "unsupported"),
        ("d2.4xlarge", &["networkInfo", "enaSupport"], "unsupported"),
        ("d3.xlarge", &["networkInfo", "enaSupport"], "required"),
        // A burstable type's vCPUs run at a baseline and spend credits to run above it; every
        // other type runs at its full rate, so it has no burst model at all.
        ("t2.small", &["cpuBurstModel"], "credit"),
        ("m6id.large", &["cpuBurstModel"], "none"),
        ("c8i-flex.8xlarge", &["cpuBurstModel"], "none"),
        // The eighth generation's connection tracking holds an established TCP connection for
        // 350 s where the earlier hardware holds it for the published 432,000 s.
        (
            "c8ib.48xlarge",
            &["networkInfo", "connectionTrackingConfiguration", "defaultTcpEstablishedTimeout"],
            "350",
        ),
        (
            "d2.xlarge",
            &["networkInfo", "connectionTrackingConfiguration", "defaultTcpEstablishedTimeout"],
            "432000",
        ),
        // Dedicated Hosts are offered on the 2015 dense HDD family and on neither its Nitro
        // successor nor the flexible eighth generation family.
        ("d2.8xlarge", &["dedicatedHostsSupported"], "true"),
        ("d3.8xlarge", &["dedicatedHostsSupported"], "false"),
        ("r5ad.24xlarge", &["dedicatedHostsSupported"], "false"),
        ("c8i-flex.8xlarge", &["dedicatedHostsSupported"], "false"),
        // The block storage optimized family of the eighth generation carries no local PTP
        // hardware clock and the packets of a session cannot be copied under its guest, while
        // an instance IS migrated on a user-initiated reboot.
        ("c8ib.48xlarge", &["phcSupport"], "unsupported"),
        ("c8ib.48xlarge", &["rebootMigrationSupport"], "supported"),
        (
            "c8ib.12xlarge",
            &["networkInfo", "trafficMirroringSupport"],
            "unsupported",
        ),
        (
            "c8ib.12xlarge",
            &["networkInfo", "secondaryNetworkSupported"],
            "false",
        ),
        (
            "c8ib.12xlarge",
            &["networkInfo", "ipv4AddressesPerSecondaryInterface"],
            "0",
        ),
        // ENA Express is offered from the `4xlarge` of that family up, the sizes under it
        // carrying less dedicated network bandwidth than its transport needs.
        ("c8ib.2xlarge", &["networkInfo", "enaSrdSupported"], "false"),
        ("c8ib.4xlarge", &["networkInfo", "enaSrdSupported"], "true"),
    ];
    for (instance_type, path, expected) in rows {
        let case = format!("{instance_type} {path:?}");
        let body = described(&case, &router, instance_type).await;
        let reported = required(&case, &body, path);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {path:?} {expected:?}, got {reported:?}"
        );
    }

    // The processor's sustained frequency is the published figure of the processor the family
    // was built on, not one its generation number gives: the burstable Xen family publishes
    // the frequency its Turbo reaches.
    for (instance_type, sustained_ghz) in [
        ("t2.small", 3.3),
        ("t2.2xlarge", 3.3),
        ("d2.xlarge", 2.4),
        ("d3.8xlarge", 3.1),
        ("r5ad.24xlarge", 2.2),
        ("c8ib.48xlarge", 3.9),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = number(case, &body, &["processorInfo", "sustainedClockSpeedInGhz"]);
        assert!(
            (reported - sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {sustained_ghz}, got {reported}"
        );
    }
}

/// Rule: the lists a type publishes — its vCPUs' cores and threads, its architectures, its
/// root device types, the bandwidth weightings it accepts and its Elastic Fabric Adapters —
/// are the published ones of that type.
#[tokio::test]
async fn published_lists_and_counts_are_reported_per_type() {
    let (router, _engine) = emulator();

    // A burstable Xen type's vCPU IS a core — it runs one thread — so it publishes its vCPUs
    // as that many cores, where a Nitro Intel type of the same width runs two threads per
    // core.
    for (instance_type, cores, threads) in [
        ("t2.medium", 2, 1),
        ("t2.large", 2, 1),
        ("t2.xlarge", 4, 1),
        ("t2.2xlarge", 8, 1),
        ("m6id.12xlarge", 24, 2),
        ("d2.8xlarge", 18, 2),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = number(case, &body, &["vCpuInfo", "defaultCores"]) as i64;
        assert_eq!(
            reported, cores,
            "case {case:?}: expected defaultCores {cores}, got {reported}"
        );
        let reported = number(case, &body, &["vCpuInfo", "defaultThreadsPerCore"]) as i64;
        assert_eq!(
            reported, threads,
            "case {case:?}: expected defaultThreadsPerCore {threads}, got {reported}"
        );
    }

    // The four smallest sizes of the burstable Xen family are on the published list of the
    // types that run 32-bit AMIs; the sizes above them are not.
    for (instance_type, architectures) in [
        ("t2.nano", vec!["i386", "x86_64"]),
        ("t2.small", vec!["i386", "x86_64"]),
        ("t2.medium", vec!["i386", "x86_64"]),
        ("t2.large", vec!["x86_64"]),
        ("t2.2xlarge", vec!["x86_64"]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported =
            reported_instance_type_list(case, &body, &["processorInfo", "supportedArchitectures"]);
        assert_eq!(
            reported, architectures,
            "case {case:?}: expected supportedArchitectures {architectures:?}, got {reported:?}"
        );
    }

    // An AMI for the 2015 dense HDD family may be backed by its local store as well as by an
    // EBS volume; its Nitro successor boots from EBS alone.
    for (instance_type, root_devices) in [
        ("d2.xlarge", vec!["ebs", "instance-store"]),
        ("d2.8xlarge", vec!["ebs", "instance-store"]),
        ("d3.8xlarge", vec!["ebs"]),
        ("m6id.large", vec!["ebs"]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = reported_instance_type_list(case, &body, &["supportedRootDeviceTypes"]);
        assert_eq!(
            reported, root_devices,
            "case {case:?}: expected supportedRootDeviceTypes {root_devices:?}, got {reported:?}"
        );
    }

    // Being a previous generation family does not withdraw the IPv6 addresses a family is
    // offered: the dense HDD family of 2015 is one of the previous generation families AWS
    // offers them on, and an interface of a type that has them takes as many as it takes
    // private IPv4 addresses.
    for instance_type in ["d2.xlarge", "d2.8xlarge", "t2.large", "m6id.large"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = required(case, &body, &["networkInfo", "ipv6Supported"]);
        assert_eq!(
            reported, "true",
            "case {case:?}: expected ipv6Supported true, got {reported:?}"
        );
        let ipv4 = number(case, &body, &["networkInfo", "ipv4AddressesPerInterface"]) as i64;
        let ipv6 = number(case, &body, &["networkInfo", "ipv6AddressesPerInterface"]) as i64;
        assert_eq!(
            ipv6, ipv4,
            "case {case:?}: expected ipv6AddressesPerInterface {ipv4}, got {ipv6}"
        );
    }

    // The flexible eighth generation family's network and EBS bandwidth can be weighted
    // towards either side, so it publishes the weightings a launch may ask for; a family whose
    // split is fixed publishes none at all.
    let case = "c8i-flex.8xlarge";
    let body = described(case, &router, case).await;
    let reported = reported_instance_type_list(case, &body, &["networkInfo", "bandwidthWeightings"]);
    assert_eq!(
        reported,
        vec!["default", "ebs-1", "vpc-1"],
        "case {case:?}: expected the three published bandwidth weightings, got {reported:?}"
    );
    for case in ["d3.8xlarge", "r5ad.24xlarge"] {
        let body = described(case, &router, case).await;
        let reported =
            reported_instance_type_member(case, &body, &["networkInfo", "bandwidthWeightings"]);
        assert!(
            reported.is_none(),
            "case {case:?}: a type whose bandwidth split is fixed publishes no weighting, got \
             {reported:?}"
        );
    }

    // An Elastic Fabric Adapter is offered on the sizes the published list names and on no
    // other, and a size offered one reports how many it carries.
    for (instance_type, adapters) in [
        ("c8ib.48xlarge", Some(1)),
        ("m6id.32xlarge", Some(1)),
        ("c8ib.8xlarge", None),
        ("m6id.12xlarge", None),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = required(case, &body, &["networkInfo", "efaSupported"]);
        let expected = if adapters.is_some() { "true" } else { "false" };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected efaSupported {expected:?}, got {reported:?}"
        );
        match adapters {
            Some(count) => {
                let reported =
                    number(case, &body, &["networkInfo", "efaInfo", "maximumEfaInterfaces"]) as i64;
                assert_eq!(
                    reported, count,
                    "case {case:?}: expected maximumEfaInterfaces {count}, got {reported}"
                );
            }
            None => {
                let reported =
                    reported_instance_type_member(case, &body, &["networkInfo", "efaInfo"]);
                assert!(
                    reported.is_none(),
                    "case {case:?}: a type offered no adapter reports no efaInfo, got \
                     {reported:?}"
                );
            }
        }
    }
}
