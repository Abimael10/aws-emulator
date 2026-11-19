// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for the AMD EPYC families, for a graphics
//! family and for a previous generation one — driven over the wire the service speaks
//! (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: EVERY MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE TYPE IS
//! THE FIGURE AWS PUBLISHES FOR THAT ONE TYPE, not a function of the size multiplier in its
//! name nor of its family's generation. The EBS bandwidth and IOPS a type sustains and the
//! figures it reaches (`EbsInfo$EbsOptimizedInfo`), whether it has an EBS-optimized mode at
//! all (`$EbsOptimizedSupport`), how many volumes it holds attached and whether that limit is
//! its own (`$MaximumEbsAttachments` / `$AttachmentLimitType`), its memory
//! (`MemoryInfo$SizeInMiB`), its local devices (`InstanceStorageInfo`), its accelerators
//! (`GpuInfo`), its processor and that processor's sustained frequency, how its vCPUs are
//! arranged into cores and threads (`VCpuInfo`), the weightings its bandwidth can be
//! configured with (`NetworkInfo$BandwidthWeightings`), the idle timeout its connection
//! tracking applies (`$ConnectionTrackingConfiguration`) and whether it is offered IPv6
//! addresses (`$Ipv6Supported`) are each a published fact of the type.
//!
//! The tables below span what the rule generalises over: five families that differ in
//! processor maker, in hardware generation, in whether simultaneous multithreading is enabled
//! at all, in carrying local devices, in carrying accelerators, in whether their EBS limit is
//! shared or their own, and in being current or previous generation — and inside each family
//! every size is a row, so no one size is the specification. The figures are the ones AWS
//! states for each type on its family's instance-type specification page, in the "Amazon EBS
//! optimized instance types", "Instance volume limits" and "IP addresses per network interface
//! per instance type" tables of the EC2 User Guide and on its "Previous generation instances"
//! page.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{described_instance_type, element, elements};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The megabits in a megabyte, which is all that separates the `Mbps` figures of
/// `EbsOptimizedInfo` from its `MB/s` ones.
const MEGABITS_PER_MEGABYTE: f64 = 8.0;

/// The body `DescribeInstanceTypes` answers for exactly one instance type, through the shared
/// harness so this suite holds no second copy of the call.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// One member of a reported type, or a panic naming the case when it is absent.
fn required(case: &str, xml: &str, name: &str) -> String {
    element(xml, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> reported, got {xml}"))
}

/// One member of a reported type as a number.
fn number(case: &str, xml: &str, name: &str) -> f64 {
    let text = required(case, xml, name);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: <{name}> must be a number, got {text:?}"))
}

/// The published row of one instance type: the figures a client sizes its compute, its storage
/// and its network from.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `c8a.12xlarge` and the like.
    instance_type: &'static str,
    /// Its memory, in MiB.
    memory_mib: i64,
    /// (vCPUs, cores, threads per core).
    vcpus: (i64, i64, i64),
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches), or `None` for a
    /// type with no EBS-optimized mode at all, which has no such performance to report.
    ebs: Option<(i64, i64, i64, i64)>,
    /// The EBS volumes it holds attached, and whether that limit is shared with its interfaces
    /// and local devices or is the volumes' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each, `ssd` or `hdd`), or `None` where it carries none.
    instance_store: Option<(i64, i64, &'static str)>,
    /// Its processor's maker and that processor's sustained frequency, in GHz.
    processor: (&'static str, f64),
    /// Is it offered IPv6 addresses?
    ipv6: bool,
}

/// Every size of five families, with the figures AWS publishes for each.
///
/// C8a: 8th generation AMD EPYC compute optimized, shipped with simultaneous multithreading
/// DISABLED (so each vCPU is a core), EBS figures dedicated from the `8xlarge` up and a volume
/// limit of the EBS devices' own that grows with the size.
/// R5a: 1st generation AMD EPYC memory optimized, whose EBS figures are its own and well under
/// the Intel ladder of the same generation, with a volume limit out of the shared pool.
/// T3a: the burstable family on that same processor, whose EBS baseline follows the credits a
/// size earns rather than its share of the host.
/// Gr6: a graphics family carrying one accelerator and local NVMe devices whatever its size.
/// M3: a PREVIOUS generation family — memory that is not a whole number of GiB per vCPU, SATA
/// SSD devices, no EBS-optimized mode at all on its two smaller sizes and no IPv6 anywhere.
const ROWS: [Row; 23] = [
    Row {
        instance_type: "c8a.medium",
        memory_mib: 2 * 1024,
        vcpus: (1, 1, 1),
        ebs: Some((315, 2_500, 10_000, 40_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "c8a.2xlarge",
        memory_mib: 16 * 1024,
        vcpus: (8, 8, 1),
        ebs: Some((2_500, 12_000, 10_000, 40_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "c8a.8xlarge",
        memory_mib: 64 * 1024,
        vcpus: (32, 32, 1),
        ebs: Some((10_000, 40_000, 10_000, 40_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "c8a.12xlarge",
        memory_mib: 96 * 1024,
        vcpus: (48, 48, 1),
        ebs: Some((15_000, 60_000, 15_000, 60_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "c8a.16xlarge",
        memory_mib: 128 * 1024,
        vcpus: (64, 64, 1),
        ebs: Some((20_000, 80_000, 20_000, 80_000)),
        ebs_attachments: (48, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "c8a.24xlarge",
        memory_mib: 192 * 1024,
        vcpus: (96, 96, 1),
        ebs: Some((30_000, 120_000, 30_000, 120_000)),
        ebs_attachments: (64, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "c8a.48xlarge",
        memory_mib: 384 * 1024,
        vcpus: (192, 192, 1),
        ebs: Some((40_000, 240_000, 40_000, 240_000)),
        ebs_attachments: (128, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "c8a.metal-24xl",
        memory_mib: 192 * 1024,
        vcpus: (96, 96, 1),
        ebs: Some((30_000, 120_000, 30_000, 120_000)),
        ebs_attachments: (39, "dedicated"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
    Row {
        instance_type: "r5a.large",
        memory_mib: 16 * 1024,
        vcpus: (2, 1, 2),
        ebs: Some((650, 3_600, 2_880, 16_000)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "r5a.xlarge",
        memory_mib: 32 * 1024,
        vcpus: (4, 2, 2),
        ebs: Some((1_085, 6_000, 2_880, 16_000)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "r5a.4xlarge",
        memory_mib: 128 * 1024,
        vcpus: (16, 8, 2),
        ebs: Some((2_880, 16_000, 2_880, 16_000)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "r5a.24xlarge",
        memory_mib: 768 * 1024,
        vcpus: (96, 48, 2),
        ebs: Some((13_570, 60_000, 13_570, 60_000)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "t3a.nano",
        memory_mib: 512,
        vcpus: (2, 1, 2),
        ebs: Some((43, 250, 2_085, 11_800)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "t3a.small",
        memory_mib: 2 * 1024,
        vcpus: (2, 1, 2),
        ebs: Some((175, 1_000, 2_085, 11_800)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "t3a.large",
        memory_mib: 8 * 1024,
        vcpus: (2, 1, 2),
        ebs: Some((695, 4_000, 2_780, 15_700)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "t3a.2xlarge",
        memory_mib: 32 * 1024,
        vcpus: (8, 4, 2),
        ebs: Some((695, 4_000, 2_780, 15_700)),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        processor: ("AMD", 2.2),
        ipv6: true,
    },
    Row {
        instance_type: "gr6.4xlarge",
        memory_mib: 128 * 1024,
        vcpus: (16, 8, 2),
        ebs: Some((8_000, 32_000, 8_000, 32_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: Some((1, 600, "ssd")),
        processor: ("AMD", 3.6),
        ipv6: true,
    },
    Row {
        instance_type: "gr6.8xlarge",
        memory_mib: 256 * 1024,
        vcpus: (32, 16, 2),
        ebs: Some((16_000, 64_000, 16_000, 64_000)),
        ebs_attachments: (32, "dedicated"),
        instance_store: Some((2, 450, "ssd")),
        processor: ("AMD", 3.6),
        ipv6: true,
    },
    Row {
        instance_type: "m3.medium",
        memory_mib: 3_840,
        vcpus: (1, 1, 1),
        ebs: None,
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 4, "ssd")),
        processor: ("Intel", 2.5),
        ipv6: false,
    },
    Row {
        instance_type: "m3.large",
        memory_mib: 7_680,
        vcpus: (2, 1, 2),
        ebs: None,
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 32, "ssd")),
        processor: ("Intel", 2.5),
        ipv6: false,
    },
    Row {
        instance_type: "m3.xlarge",
        memory_mib: 15_360,
        vcpus: (4, 2, 2),
        ebs: Some((500, 4_000, 500, 4_000)),
        ebs_attachments: (39, "shared"),
        instance_store: Some((2, 40, "ssd")),
        processor: ("Intel", 2.5),
        ipv6: false,
    },
    Row {
        instance_type: "m3.2xlarge",
        memory_mib: 30_720,
        vcpus: (8, 4, 2),
        ebs: Some((1_000, 8_000, 1_000, 8_000)),
        ebs_attachments: (39, "shared"),
        instance_store: Some((2, 80, "ssd")),
        processor: ("Intel", 2.5),
        ipv6: false,
    },
    Row {
        instance_type: "c8a.metal-48xl",
        memory_mib: 384 * 1024,
        vcpus: (192, 192, 1),
        ebs: Some((40_000, 240_000, 40_000, 240_000)),
        ebs_attachments: (79, "shared"),
        instance_store: None,
        processor: ("AMD", 4.5),
        ipv6: true,
    },
];

/// Rule, row by row: every member is the figure published for THAT instance type.
#[tokio::test]
async fn every_published_row_is_reported_for_its_own_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let memory = required(case, &body, "memoryInfo");
        let reported = number(case, &memory, "sizeInMiB") as i64;
        assert_eq!(
            reported, row.memory_mib,
            "case {case:?}: expected memoryInfo.sizeInMiB {}, got {reported}",
            row.memory_mib
        );

        let vcpu_info = required(case, &body, "vCpuInfo");
        let (vcpus, cores, threads) = row.vcpus;
        for (name, expected) in [
            ("defaultVCpus", vcpus),
            ("defaultCores", cores),
            ("defaultThreadsPerCore", threads),
        ] {
            let reported = number(case, &vcpu_info, name) as i64;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {name} {expected}, got {reported}"
            );
        }
        // One thread per core is the only count a type with multithreading disabled can be
        // configured with; a type that runs two can also be configured down to one.
        let threads_per_core = required(case, &vcpu_info, "validThreadsPerCore");
        let configurable = elements(&threads_per_core, "item");
        assert_eq!(
            configurable.len(),
            threads as usize,
            "case {case:?}: expected {threads} valid threads-per-core counts, got {configurable:?}"
        );

        let ebs_info = required(case, &body, "ebsInfo");
        match row.ebs {
            Some((baseline_mbps, baseline_iops, maximum_mbps, maximum_iops)) => {
                let performance = required(case, &ebs_info, "ebsOptimizedInfo");
                for (name, expected) in [
                    ("baselineBandwidthInMbps", baseline_mbps),
                    ("baselineIops", baseline_iops),
                    ("maximumBandwidthInMbps", maximum_mbps),
                    ("maximumIops", maximum_iops),
                ] {
                    let reported = number(case, &performance, name) as i64;
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected {name} {expected}, got {reported}"
                    );
                }
                // The throughput members are the bandwidth ones in the other unit, which is how
                // the published table's Mbps and MB/s columns relate.
                for (name, bandwidth) in [
                    ("baselineThroughputInMBps", baseline_mbps),
                    ("maximumThroughputInMBps", maximum_mbps),
                ] {
                    let expected = bandwidth as f64 / MEGABITS_PER_MEGABYTE;
                    let reported = number(case, &performance, name);
                    assert!(
                        (reported - expected).abs() < 0.001,
                        "case {case:?}: expected {name} {expected}, got {reported}"
                    );
                }
            }
            None => {
                let support = required(case, &ebs_info, "ebsOptimizedSupport");
                assert_eq!(
                    support, "unsupported",
                    "case {case:?}: a type with no EBS-optimized mode publishes it as \
                     unsupported, got {support:?}"
                );
                assert!(
                    element(&ebs_info, "ebsOptimizedInfo").is_none(),
                    "case {case:?}: a type with no EBS-optimized mode reports no \
                     ebsOptimizedInfo, got {ebs_info}"
                );
            }
        }

        let (attachments, limit_type) = row.ebs_attachments;
        let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
        assert_eq!(
            reported, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {reported}"
        );
        let reported = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            reported, limit_type,
            "case {case:?}: expected attachmentLimitType {limit_type:?}, got {reported:?}"
        );

        let storage = element(&body, "instanceStorageInfo");
        match row.instance_store {
            Some((devices, gb_each, disk_type)) => {
                let storage = storage.unwrap_or_else(|| {
                    panic!("case {case:?}: expected an instanceStorageInfo, got {body}")
                });
                let disks = required(case, &storage, "disks");
                for (name, expected) in [("count", devices), ("sizeInGB", gb_each)] {
                    let reported = number(case, &disks, name) as i64;
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected local devices' {name} {expected}, got {reported}"
                    );
                }
                let reported = required(case, &disks, "type");
                assert_eq!(
                    reported, disk_type,
                    "case {case:?}: expected local devices of type {disk_type:?}, got {reported:?}"
                );
                let reported = number(case, &storage, "totalSizeInGB") as i64;
                assert_eq!(
                    reported,
                    devices * gb_each,
                    "case {case:?}: expected totalSizeInGB {}, got {reported}",
                    devices * gb_each
                );
            }
            None => assert!(
                storage.is_none(),
                "case {case:?}: a type with no local devices reports no instanceStorageInfo, \
                 got {storage:?}"
            ),
        }

        let processor_info = required(case, &body, "processorInfo");
        let (manufacturer, sustained_ghz) = row.processor;
        let reported = required(case, &processor_info, "manufacturer");
        assert_eq!(
            reported, manufacturer,
            "case {case:?}: expected processor manufacturer {manufacturer:?}, got {reported:?}"
        );
        let reported = number(case, &processor_info, "sustainedClockSpeedInGhz");
        assert!(
            (reported - sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {sustained_ghz}, got {reported}"
        );

        // A type AWS offers no IPv6 address reports a limit of none per interface.
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "ipv6Supported");
        assert_eq!(
            reported,
            row.ipv6.to_string(),
            "case {case:?}: expected ipv6Supported {}, got {reported:?}",
            row.ipv6
        );
        let addresses = number(case, &network_info, "ipv6AddressesPerInterface") as i64;
        assert_eq!(
            addresses > 0,
            row.ipv6,
            "case {case:?}: an IPv6 address limit above none is reported exactly where IPv6 is \
             offered, got {addresses}"
        );
    }
}

/// Rule, as an invariant over every row: a type sustains no more than it reaches, and the
/// published figures do not follow the size multiplier — two families disagree at one size,
/// and one family's own figures do not double with its ladder.
#[tokio::test]
async fn the_published_figures_are_bounded_and_do_not_follow_the_size_multiplier() {
    let (router, _engine) = super::ec2_query::emulator();

    let bandwidth = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumBandwidthInMbps") as i64,
        )
    };

    for row in ROWS {
        if row.ebs.is_none() {
            continue;
        }
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let (sustained, reached) = bandwidth(&body, case);
        assert!(
            sustained > 0 && sustained <= reached,
            "case {case:?}: a type sustains at most what it reaches, got {sustained}/{reached}"
        );
    }

    // One family, two sizes whose vCPUs double: the burstable family's baseline follows the
    // credits a size earns and stops growing at the `large`, so the `2xlarge` does not carry
    // twice the `large`'s figure.
    let left = described("t3a.large", &router, "t3a.large").await;
    let right = described("t3a.2xlarge", &router, "t3a.2xlarge").await;
    let (small, _) = bandwidth(&left, "t3a.large");
    let (large, _) = bandwidth(&right, "t3a.2xlarge");
    assert_eq!(
        small, large,
        "case \"t3a\": the family's baseline stops growing at the large size, got {small} then \
         {large}"
    );

    // Two families, ONE size: every figure of the pair differs, so the size cannot be what
    // produced either.
    let left = described("c8a.4xlarge", &router, "c8a.4xlarge").await;
    let right = described("r5a.4xlarge", &router, "r5a.4xlarge").await;
    assert_ne!(
        bandwidth(&left, "c8a.4xlarge"),
        bandwidth(&right, "r5a.4xlarge"),
        "two families of one size publish different EBS figures"
    );
    let left_vcpu = required("c8a.4xlarge", &left, "vCpuInfo");
    let right_vcpu = required("r5a.4xlarge", &right, "vCpuInfo");
    assert_ne!(
        required("c8a.4xlarge", &left_vcpu, "defaultCores"),
        required("r5a.4xlarge", &right_vcpu, "defaultCores"),
        "a family with multithreading disabled reports more cores for the same vCPUs than one \
         that runs two threads per core"
    );
}

/// Rule: a type AWS publishes accelerators for reports the inventory — how many devices, which
/// device, how much memory each carries and the total — and a type with none reports no
/// `gpuInfo` at all.
#[tokio::test]
async fn an_accelerated_type_reports_its_accelerators() {
    let (router, _engine) = super::ec2_query::emulator();

    for (case, devices) in [("gr6.4xlarge", 1), ("gr6.8xlarge", 1)] {
        let body = described(case, &router, case).await;
        let gpu_info = required(case, &body, "gpuInfo");
        let gpus = required(case, &gpu_info, "gpus");
        let reported = number(case, &gpus, "count") as i64;
        assert_eq!(
            reported, devices,
            "case {case:?}: expected {devices} accelerators, got {reported}"
        );
        let reported = required(case, &gpus, "manufacturer");
        assert_eq!(
            reported, "NVIDIA",
            "case {case:?}: expected accelerator manufacturer NVIDIA, got {reported:?}"
        );
        let memory = required(case, &gpus, "memoryInfo");
        let each = number(case, &memory, "sizeInMiB") as i64;
        // The accelerator the sixth generation graphics families carry, and its published
        // memory: a 24 GB device.
        let name = required(case, &gpus, "name");
        assert_eq!(
            name, "L4",
            "case {case:?}: expected the published accelerator name L4, got {name:?}"
        );
        assert_eq!(
            each, 22_888,
            "case {case:?}: expected 22888 MiB of accelerator memory per device, got {each}"
        );
        let total = number(case, &gpu_info, "totalGpuMemoryInMiB") as i64;
        assert_eq!(
            total,
            each * devices,
            "case {case:?}: the total accelerator memory is each device's times the count, got \
             {total} for {devices} of {each}"
        );
    }

    // The same family's compute counterpart carries no accelerator, so it reports none.
    let case = "c8a.4xlarge";
    let body = described(case, &router, case).await;
    assert!(
        element(&body, "gpuInfo").is_none(),
        "case {case:?}: a type with no accelerator reports no gpuInfo, got {body}"
    );
}

/// Rule: a type whose network and EBS bandwidth can be weighted towards one or the other
/// publishes the weightings it accepts and the shorter established-TCP idle timeout its
/// hardware applies; a type whose bandwidth is fixed publishes no weighting at all and the
/// timeout every earlier generation reports.
#[tokio::test]
async fn configurable_bandwidth_and_connection_tracking_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    let case = "c8a.16xlarge";
    let body = described(case, &router, case).await;
    let network_info = required(case, &body, "networkInfo");
    let weightings = elements(&required(case, &network_info, "bandwidthWeightings"), "item");
    assert_eq!(
        weightings,
        vec!["default", "ebs-1", "vpc-1"],
        "case {case:?}: expected the published bandwidth weightings, got {weightings:?}"
    );
    let tracking = required(case, &network_info, "connectionTrackingConfiguration");
    let reported = number(case, &tracking, "defaultTcpEstablishedTimeout") as i64;
    assert_eq!(
        reported, 350,
        "case {case:?}: expected defaultTcpEstablishedTimeout 350, got {reported}"
    );

    for case in ["r5a.4xlarge", "m3.xlarge"] {
        let body = described(case, &router, case).await;
        let network_info = required(case, &body, "networkInfo");
        assert!(
            element(&network_info, "bandwidthWeightings").is_none(),
            "case {case:?}: a type whose bandwidth split is fixed publishes no weighting, got \
             {network_info}"
        );
        let tracking = required(case, &network_info, "connectionTrackingConfiguration");
        let reported = number(case, &tracking, "defaultTcpEstablishedTimeout") as i64;
        assert_eq!(
            reported, 432_000,
            "case {case:?}: expected defaultTcpEstablishedTimeout 432000, got {reported}"
        );
    }
}

/// Rule: IPv6 is offered on every current generation instance type and on the previous
/// generation families AWS names beside them — C3, I2 and R3 — so a previous generation family
/// that list leaves out reports no IPv6 support and no IPv6 address limit, however new the rest
/// of its capabilities are. The rows below span both sides of that list and both generations.
#[tokio::test]
async fn ipv6_is_offered_to_the_published_families_alone() {
    let (router, _engine) = super::ec2_query::emulator();

    for (case, offered) in [
        ("c8a.large", true),
        ("t3a.micro", true),
        ("c3.large", true),
        ("i2.xlarge", true),
        ("r3.large", true),
        ("m3.large", false),
        ("m1.small", false),
        ("c1.medium", false),
    ] {
        let body = described(case, &router, case).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "ipv6Supported");
        assert_eq!(
            reported,
            offered.to_string(),
            "case {case:?}: expected ipv6Supported {offered}, got {reported:?}"
        );
        let addresses = number(case, &network_info, "ipv6AddressesPerInterface") as i64;
        assert_eq!(
            addresses > 0,
            offered,
            "case {case:?}: an IPv6 address limit above none is reported exactly where IPv6 is \
             offered, got {addresses}"
        );
    }

    // Amazon EBS encryption reaches a LONGER list of previous generation families than IPv6
    // does, so the two members are not one answer: this family's volumes can be encrypted
    // while it is offered no IPv6 address at all.
    let case = "m3.xlarge";
    let body = described(case, &router, case).await;
    let ebs_info = required(case, &body, "ebsInfo");
    let encryption = required(case, &ebs_info, "encryptionSupport");
    assert_eq!(
        encryption, "supported",
        "case {case:?}: expected encryptionSupport supported, got {encryption:?}"
    );
}

/// Rule: one instance type has ONE published row. The rows are grouped into a table per
/// concern — the families above live in three of them — and a row is reached by its instance
/// type alone, so a type written into two tables would be a row nothing ever reads.
#[tokio::test]
async fn every_published_row_names_a_type_no_other_row_names() {
    let mut seen: Vec<&'static str> = Vec::new();
    for instance_type in service_com_amazonaws_ec2_amazonec2::state::published_instance_types() {
        assert!(
            !seen.contains(&instance_type),
            "case {instance_type:?}: two published rows name one instance type"
        );
        seen.push(instance_type);
    }
    // Every row of the tables this fix adds is reachable, which is what makes the split safe.
    for row in ROWS {
        assert!(
            seen.contains(&row.instance_type),
            "case {:?}: the published tables must hold this row, got {} rows",
            row.instance_type,
            seen.len()
        );
    }
}

/// Rule: a reporting operation does not change what a later caller sees — the catalog is a
/// fact, so two calls about one type answer identically.
#[tokio::test]
async fn one_instance_type_is_reported_the_same_way_twice() {
    let (router, _engine) = super::ec2_query::emulator();

    for case in ["c8a.12xlarge", "m3.large", "gr6.4xlarge"] {
        // The reported type itself, without the envelope's own per-request id.
        let first = required(case, &described(case, &router, case).await, "instanceTypeSet");
        let second = required(case, &described(case, &router, case).await, "instanceTypeSet");
        assert_eq!(
            first, second,
            "case {case:?}: the published row of one type is the same on every read"
        );
    }
}
