// SPDX-License-Identifier: CC0-1.0

//! The published per-type rows of five instance families whose hardware no function of the
//! type's name yields — driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: EVERY MEMBER `DescribeInstanceTypes` REPORTS THAT AWS PUBLISHES PER
//! INSTANCE TYPE IS THE FIGURE PUBLISHED FOR THAT ONE TYPE, never a function of the size
//! multiplier in its name nor a single default for the whole catalog. That covers the EBS
//! bandwidth and IOPS a type sustains and the ones it reaches (`EbsInfo$EbsOptimizedInfo`),
//! whether it has an EBS-optimized mode at all (`$EbsOptimizedSupport`), how many volumes it
//! holds attached and whether that limit is its own (`$MaximumEbsAttachments` /
//! `$AttachmentLimitType`), its local devices (`InstanceStorageInfo$Disks` /
//! `$TotalSizeInGB`), its memory (`MemoryInfo$SizeInMiB`), its network performance and
//! address limits (`NetworkInfo$NetworkPerformance` / `$BandwidthWeightings` /
//! `$EnaSrdSupported` / `$FlexibleEnaQueuesSupport`), its processor's sustained frequency and
//! features (`ProcessorInfo$SustainedClockSpeedInGhz` / `$SupportedFeatures`), the core
//! counts it can be configured with (`VCpuInfo$ValidCores`), the placement group strategies
//! and root device types it is offered (`PlacementGroupInfo$SupportedStrategies` /
//! `InstanceTypeInfo$SupportedRootDeviceTypes`) and the host features it is offered
//! (Dedicated Hosts, CloudWatch action based recovery, reboot migration, a PTP hardware
//! clock, Traffic Mirroring, Nitro Enclaves).
//!
//! The table spans what the rule generalises over: five families of four different hardware
//! designs (AWS Graviton4 with and without local NVMe, 3rd and 4th generation Intel Xeon
//! Scalable, and a previous generation Xen family), and within each family rows that differ
//! in size, in device count, in device size, in processor frequency, in network performance,
//! in attachment limit and in which host features they are offered — so no one row is the
//! specification. Each family's figures are the ones AWS states for each type on its
//! family's instance-type specification page, in the "Amazon EBS optimized instance types"
//! and "Instance volume limits" tables, in the "IP addresses per network interface per
//! instance type" table and in the "CPU cores and threads per CPU core per instance type"
//! table of the EC2 User Guide; they are recorded in the service's published catalog
//! (`src/state/instance_ids_and_instance_types/instance_type_catalog.rs`), whose row is what
//! this suite reads back over the wire.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, indexed};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The megabits in a megabyte, which is all that separates the `Mbps` figures of
/// `EbsOptimizedInfo` from its `MB/s` ones.
const MEGABITS_PER_MEGABYTE: f64 = 8.0;

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeInstanceTypes",
        &indexed("InstanceType", &[instance_type]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {instance_type}, got {status} with body {body}"
    );
    body
}

/// One member of a reported type, or a panic naming the case when it is absent.
fn required(case: &str, xml: &str, name: &str) -> String {
    element(xml, name)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> reported, got {xml}"))
}

/// One member of a reported type as a number.
fn number(case: &str, xml: &str, name: &str) -> f64 {
    let text = required(case, xml, name);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: <{name}> must be a number, got {text:?}"))
}

/// The published row of one instance type: the figures a client sizing its storage, its EBS
/// throughput, its memory and its network reads.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `c8g.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches), or `None` for a
    /// type with no EBS-optimized mode at all, which has no such performance to report.
    ebs: Option<(i64, i64, i64, i64)>,
    /// Whether the type has an EBS-optimized mode: one value of `EbsOptimizedSupport`.
    ebs_optimized_support: &'static str,
    /// The EBS volumes it holds attached, and whether that limit is shared with its
    /// interfaces and local devices or is the volumes' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its memory in MiB, where the family publishes a figure of its own rather than a whole
    /// number of GiB per vCPU; `None` leaves the family ratio unasserted here.
    memory_mib: Option<i64>,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its processor's sustained frequency, in GHz.
    sustained_ghz: f64,
}

/// Rows from five families, chosen so that every column differs across the table.
///
/// C8g and C8gd are ONE Graviton4 design, so their EBS figures agree size for size while
/// only the local devices differ; I4i's volumes come out of the shared Nitro pool its own
/// devices eat into, so its limit FALLS as the devices grow, where the two Graviton4
/// families and M7i publish a limit of their own that RISES with the size; and R3 — a
/// previous generation Xen family — publishes one fixed EBS figure per size where it has an
/// EBS-optimized mode at all, no such figure on the two sizes that have none, and a memory
/// figure of its own at every size.
const ROWS: [Row; 17] = [
    Row {
        instance_type: "c8g.medium",
        ebs: Some((315, 2_500, 10_000, 40_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        memory_mib: Some(2 * 1_024),
        network_performance: "Up to 12.5 Gigabit",
        sustained_ghz: 2.8,
    },
    Row {
        instance_type: "c8g.4xlarge",
        ebs: Some((5_000, 20_000, 10_000, 40_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        memory_mib: Some(32 * 1_024),
        network_performance: "Up to 15 Gigabit",
        sustained_ghz: 2.8,
    },
    Row {
        instance_type: "c8g.12xlarge",
        ebs: Some((15_000, 60_000, 15_000, 60_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        memory_mib: Some(96 * 1_024),
        network_performance: "22.5 Gigabit",
        sustained_ghz: 2.8,
    },
    Row {
        instance_type: "c8g.metal-24xl",
        ebs: Some((30_000, 120_000, 30_000, 120_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (39, "dedicated"),
        instance_store: None,
        memory_mib: Some(192 * 1_024),
        network_performance: "40 Gigabit",
        sustained_ghz: 2.8,
    },
    Row {
        instance_type: "c8g.48xlarge",
        ebs: Some((40_000, 240_000, 40_000, 240_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (128, "dedicated"),
        instance_store: None,
        memory_mib: Some(384 * 1_024),
        network_performance: "50 Gigabit",
        sustained_ghz: 2.7,
    },
    Row {
        instance_type: "c8gd.large",
        ebs: Some((630, 3_600, 10_000, 40_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (32, "dedicated"),
        instance_store: Some((1, 118)),
        memory_mib: Some(4 * 1_024),
        network_performance: "Up to 12.5 Gigabit",
        sustained_ghz: 2.8,
    },
    Row {
        instance_type: "c8gd.12xlarge",
        ebs: Some((15_000, 60_000, 15_000, 60_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (32, "dedicated"),
        instance_store: Some((3, 950)),
        memory_mib: Some(96 * 1_024),
        network_performance: "22.5 Gigabit",
        sustained_ghz: 2.8,
    },
    Row {
        instance_type: "c8gd.metal-48xl",
        ebs: Some((40_000, 240_000, 40_000, 240_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (79, "dedicated"),
        instance_store: Some((6, 1_900)),
        memory_mib: Some(384 * 1_024),
        network_performance: "50 Gigabit",
        sustained_ghz: 2.7,
    },
    Row {
        instance_type: "i4i.large",
        ebs: Some((625, 2_500, 10_000, 40_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 468)),
        memory_mib: Some(16 * 1_024),
        network_performance: "Up to 10 Gigabit",
        sustained_ghz: 3.5,
    },
    Row {
        instance_type: "i4i.16xlarge",
        ebs: Some((20_000, 80_000, 20_000, 80_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (23, "shared"),
        instance_store: Some((4, 3_750)),
        memory_mib: Some(512 * 1_024),
        network_performance: "37.5 Gigabit",
        sustained_ghz: 3.5,
    },
    Row {
        instance_type: "i4i.metal",
        ebs: Some((40_000, 160_000, 40_000, 160_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (31, "shared"),
        instance_store: Some((8, 3_750)),
        memory_mib: Some(1_024 * 1_024),
        network_performance: "75 Gigabit",
        sustained_ghz: 3.5,
    },
    Row {
        instance_type: "m7i.large",
        ebs: Some((650, 3_600, 10_000, 40_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        memory_mib: Some(8 * 1_024),
        network_performance: "Up to 12.5 Gigabit",
        sustained_ghz: 3.2,
    },
    Row {
        instance_type: "m7i.24xlarge",
        ebs: Some((30_000, 120_000, 30_000, 120_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (64, "dedicated"),
        instance_store: None,
        memory_mib: Some(384 * 1_024),
        network_performance: "37.5 Gigabit",
        sustained_ghz: 3.2,
    },
    Row {
        instance_type: "m7i.metal-48xl",
        ebs: Some((40_000, 240_000, 40_000, 240_000)),
        ebs_optimized_support: "default",
        ebs_attachments: (79, "dedicated"),
        instance_store: None,
        memory_mib: Some(768 * 1_024),
        network_performance: "50 Gigabit",
        sustained_ghz: 3.2,
    },
    Row {
        instance_type: "r3.xlarge",
        ebs: Some((500, 4_000, 500, 4_000)),
        ebs_optimized_support: "supported",
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 80)),
        memory_mib: Some(31_232),
        network_performance: "Moderate",
        sustained_ghz: 2.5,
    },
    Row {
        instance_type: "r3.2xlarge",
        ebs: Some((1_000, 8_000, 1_000, 8_000)),
        ebs_optimized_support: "supported",
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 160)),
        memory_mib: Some(62_464),
        network_performance: "High",
        sustained_ghz: 2.5,
    },
    Row {
        instance_type: "r3.8xlarge",
        ebs: None,
        ebs_optimized_support: "unsupported",
        ebs_attachments: (39, "shared"),
        instance_store: Some((2, 320)),
        memory_mib: Some(249_856),
        network_performance: "10 Gigabit",
        sustained_ghz: 2.5,
    },
];

/// Rule: every published figure of a row reaches the caller as that row's own value.
#[tokio::test]
async fn published_rows_report_the_hardware_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let ebs_info = required(case, &body, "ebsInfo");
        let reported = required(case, &ebs_info, "ebsOptimizedSupport");
        assert_eq!(
            reported, row.ebs_optimized_support,
            "case {case:?}: expected ebsOptimizedSupport {:?}, got {reported:?}",
            row.ebs_optimized_support
        );

        match row.ebs {
            Some((baseline_mbps, baseline_iops, maximum_mbps, maximum_iops)) => {
                let performance = required(case, &ebs_info, "ebsOptimizedInfo");
                for (member, expected) in [
                    ("baselineBandwidthInMbps", baseline_mbps),
                    ("baselineIops", baseline_iops),
                    ("maximumBandwidthInMbps", maximum_mbps),
                    ("maximumIops", maximum_iops),
                ] {
                    let reported = number(case, &performance, member) as i64;
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected {member} {expected}, got {reported}"
                    );
                }
                // The throughput columns are the bandwidth ones in the other unit.
                for (member, mbps) in [
                    ("baselineThroughputInMBps", baseline_mbps),
                    ("maximumThroughputInMBps", maximum_mbps),
                ] {
                    let reported = number(case, &performance, member);
                    let expected = mbps as f64 / MEGABITS_PER_MEGABYTE;
                    assert!(
                        (reported - expected).abs() < 0.001,
                        "case {case:?}: expected {member} {expected}, got {reported}"
                    );
                }
            }
            None => assert!(
                element(&ebs_info, "ebsOptimizedInfo").is_none(),
                "case {case:?}: a type with no EBS-optimized mode reports no \
                 ebsOptimizedInfo, got {ebs_info}"
            ),
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

        match row.instance_store {
            Some((devices, gb_each)) => {
                let storage = required(case, &body, "instanceStorageInfo");
                let disks = required(case, &storage, "disks");
                let reported = number(case, &disks, "count") as i64;
                assert_eq!(
                    reported, devices,
                    "case {case:?}: expected {devices} local devices, got {reported}"
                );
                let reported = number(case, &disks, "sizeInGB") as i64;
                assert_eq!(
                    reported, gb_each,
                    "case {case:?}: expected local devices of {gb_each} GB, got {reported}"
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
                element(&body, "instanceStorageInfo").is_none(),
                "case {case:?}: a type with no local devices reports no instanceStorageInfo, \
                 got {body}"
            ),
        }

        if let Some(expected) = row.memory_mib {
            let memory_info = required(case, &body, "memoryInfo");
            let reported = number(case, &memory_info, "sizeInMiB") as i64;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected sizeInMiB {expected}, got {reported}"
            );
        }

        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "networkPerformance");
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );

        let processor_info = required(case, &body, "processorInfo");
        let reported = number(case, &processor_info, "sustainedClockSpeedInGhz");
        assert!(
            (reported - row.sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {}, got {reported}",
            row.sustained_ghz
        );
    }
}

/// Rule, per member: the host features and adapter settings a type is offered are published
/// per type — a family's local devices, its hardware generation and its hypervisor each
/// decide a different one, so two sizes of one family and two families of one generation
/// disagree.
#[tokio::test]
async fn published_rows_report_the_host_features_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: (instance type, member, expected) — one claim per member, read on types whose
    // answers differ.
    let feature_rows: [(&str, &str, &str); 24] = [
        // A store-less Graviton4 size is offered CloudWatch action based recovery on its bare
        // metal size and reboot migration everywhere; the same hardware carrying local NVMe is
        // offered neither, a reboot migration having nowhere to carry the local data.
        ("c8g.metal-24xl", "autoRecoverySupported", "true"),
        ("c8g.12xlarge", "rebootMigrationSupport", "supported"),
        ("c8gd.12xlarge", "autoRecoverySupported", "false"),
        ("c8gd.12xlarge", "rebootMigrationSupport", "unsupported"),
        // The PTP hardware clock of that generation reaches both, the bare metal sizes
        // included.
        ("c8g.12xlarge", "phcSupport", "supported"),
        ("c8gd.metal-48xl", "phcSupport", "supported"),
        // The 3rd generation Intel storage family is offered neither feature at any size.
        ("i4i.16xlarge", "rebootMigrationSupport", "unsupported"),
        ("i4i.16xlarge", "phcSupport", "unsupported"),
        // Dedicated Hosts are published per SIZE: the previous generation Xen family is
        // offered them at every size, while two sizes of the Intel families are not.
        ("r3.large", "dedicatedHostsSupported", "true"),
        ("r3.8xlarge", "dedicatedHostsSupported", "true"),
        ("i4i.24xlarge", "dedicatedHostsSupported", "false"),
        ("i4i.16xlarge", "dedicatedHostsSupported", "true"),
        ("m7i.24xlarge", "dedicatedHostsSupported", "false"),
        ("m7i.16xlarge", "dedicatedHostsSupported", "true"),
        // CloudWatch action based recovery is offered on every size of the Xen family,
        // instance store and all.
        ("r3.large", "autoRecoverySupported", "true"),
        ("r3.2xlarge", "autoRecoverySupported", "true"),
        // An enclave is carved out of two vCPUs up on this architecture, where an x86 type
        // needs four.
        ("c8g.large", "nitroEnclavesSupport", "supported"),
        ("c8gd.large", "nitroEnclavesSupport", "supported"),
        // Copying a session's packets is the Nitro System's to do, so a Xen family is offered
        // no Traffic Mirroring, and neither is this Graviton4 hardware; the Intel storage
        // family is.
        ("r3.2xlarge", "trafficMirroringSupport", "unsupported"),
        ("c8g.12xlarge", "trafficMirroringSupport", "unsupported"),
        ("i4i.16xlarge", "trafficMirroringSupport", "supported"),
        // ENA Express is published per size: offered on the sizes that carry a family's
        // dedicated network share and not below it.
        ("m7i.8xlarge", "enaSrdSupported", "false"),
        ("m7i.12xlarge", "enaSrdSupported", "true"),
        ("i4i.8xlarge", "enaSrdSupported", "true"),
    ];

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        // A member of networkInfo is read out of that block; the rest are the type's own.
        let scope = if matches!(
            member,
            "trafficMirroringSupport" | "enaSrdSupported" | "flexibleEnaQueuesSupport"
        ) {
            required(&case, &body, "networkInfo")
        } else {
            body
        };
        let reported = required(&case, &scope, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, for the members that are LISTS: the bandwidth weightings, processor features,
/// placement group strategies and root device types a type publishes are its own — and a
/// type that publishes none at all reports the member absent or empty rather than another
/// type's list.
#[tokio::test]
async fn published_rows_report_the_lists_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // A Graviton4 family's network and EBS bandwidth can be weighted towards either side, so
    // it publishes the weightings a launch may ask for; the Intel general purpose family's
    // split is fixed, so it publishes none at all.
    for (instance_type, expected) in [
        ("c8g.12xlarge", vec!["default", "ebs-1", "vpc-1"]),
        ("c8gd.large", vec!["default", "ebs-1", "vpc-1"]),
        ("m7i.12xlarge", Vec::new()),
        ("r3.2xlarge", Vec::new()),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = element(&network_info, "bandwidthWeightings")
            .map(|weightings| elements(&weightings, "item"))
            .unwrap_or_default();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected bandwidthWeightings {expected:?}, got {reported:?}"
        );
    }

    // The 4th generation Intel processor runs a hypervisor inside the guest, which it
    // publishes as a processor feature; the Graviton4 and Xen families publish none.
    for (instance_type, expected) in [
        ("m7i.large", vec!["nested-virtualization"]),
        ("m7i.metal-48xl", vec!["nested-virtualization"]),
        ("c8g.12xlarge", Vec::new()),
        ("i4i.large", Vec::new()),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let processor_info = required(case, &body, "processorInfo");
        let reported = element(&processor_info, "supportedFeatures")
            .map(|features| elements(&features, "item"))
            .unwrap_or_default();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedFeatures {expected:?}, got {reported:?}"
        );
    }

    // The previous generation family publishes the cluster strategy its generation
    // introduced beside the two every type takes, and its AMIs may be backed by either an EBS
    // volume or its local instance store; a burstable family of a later generation is offered
    // no cluster group at all, and every modern family boots from EBS alone.
    for (instance_type, strategies, root_devices) in [
        (
            "r3.2xlarge",
            vec!["cluster", "partition", "spread"],
            vec!["ebs", "instance-store"],
        ),
        (
            "r3.8xlarge",
            vec!["cluster", "partition", "spread"],
            vec!["ebs", "instance-store"],
        ),
        (
            "c8g.12xlarge",
            vec!["cluster", "partition", "spread"],
            vec!["ebs"],
        ),
        ("t3.large", vec!["partition", "spread"], vec!["ebs"]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let placement = required(case, &body, "placementGroupInfo");
        let reported = elements(&required(case, &placement, "supportedStrategies"), "item");
        assert_eq!(
            reported, strategies,
            "case {case:?}: expected supportedStrategies {strategies:?}, got {reported:?}"
        );
        let reported = elements(
            &required(case, &body, "supportedRootDeviceTypes"),
            "item",
        );
        assert_eq!(
            reported, root_devices,
            "case {case:?}: expected supportedRootDeviceTypes {root_devices:?}, got \
             {reported:?}"
        );
    }

    // The core counts a type can be configured with are its own: the size that holds the
    // whole host takes its cores all at once, a size whose cores sit on two processor sockets
    // takes them one per socket, and a size on one socket takes any count up to its default.
    for (instance_type, expected) in [
        ("c8g.48xlarge", vec![192]),
        ("m7i.48xlarge", vec![96]),
        ("r3.8xlarge", (1..=8).map(|step| step * 2).collect()),
        ("c8g.large", vec![1, 2]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let v_cpu_info = required(case, &body, "vCpuInfo");
        let valid_cores = required(case, &v_cpu_info, "validCores");
        let reported: Vec<i64> = elements(&valid_cores, "item")
            .iter()
            .map(|count| {
                count.parse().unwrap_or_else(|_| {
                    panic!("case {case:?}: a core count must be a number, got {count:?}")
                })
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected validCores {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, for the secondary network: whether a type takes an interface attachment from a
/// secondary network is published per type, and so is the number of private addresses one
/// such interface takes — a type that takes none publishes no secondary interface limit at
/// all while still publishing those addresses as none.
#[tokio::test]
async fn published_rows_report_the_secondary_network_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: the four families whose rows publish an answer of their own, one size each, and a
    // type the catalog holds no row for, which keeps its hardware generation's answer.
    for (instance_type, supported, ipv4_per_secondary) in [
        ("c8g.12xlarge", false, Some(0)),
        ("c8gd.large", false, Some(0)),
        ("m7i.12xlarge", false, Some(0)),
        ("m7i.metal-48xl", false, Some(0)),
        ("i4i.16xlarge", false, None),
        ("m7g.4xlarge", true, Some(30)),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "secondaryNetworkSupported");
        assert_eq!(
            reported,
            supported.to_string(),
            "case {case:?}: expected secondaryNetworkSupported {supported}, got {reported:?}"
        );
        let limit = element(&network_info, "maximumSecondaryNetworkInterfaces");
        assert_eq!(
            limit.is_some(),
            supported,
            "case {case:?}: a type that takes no secondary attachment reports no \
             maximumSecondaryNetworkInterfaces, got {limit:?}"
        );
        let reported = element(&network_info, "ipv4AddressesPerSecondaryInterface")
            .map(|addresses| {
                addresses.parse::<i64>().unwrap_or_else(|_| {
                    panic!("case {case:?}: the address count must be a number, got {addresses:?}")
                })
            });
        assert_eq!(
            reported, ipv4_per_secondary,
            "case {case:?}: expected ipv4AddressesPerSecondaryInterface \
             {ipv4_per_secondary:?}, got {reported:?}"
        );
    }
}

/// Rule, as an invariant over every row: a type sustains no more than it reaches, and the
/// published figures do not follow the size multiplier — one Graviton4 design publishes one
/// EBS figure for two families whose local devices differ, the EBS attachment limit moves in
/// OPPOSITE directions with the size on two families, and the catalog's own derivation
/// answers a type the table holds no row for.
#[tokio::test]
async fn the_published_figures_do_not_follow_the_size_multiplier() {
    let (router, _engine) = super::ec2_query::emulator();

    let bandwidth = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumBandwidthInMbps") as i64,
        )
    };
    let attachments = |body: &str, case: &str| -> i64 {
        let ebs_info = required(case, body, "ebsInfo");
        number(case, &ebs_info, "maximumEbsAttachments") as i64
    };

    for row in ROWS.iter().filter(|row| row.ebs.is_some()) {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let (sustained, reached) = bandwidth(&body, case);
        assert!(
            sustained > 0 && sustained <= reached,
            "case {case:?}: a type sustains at most what it reaches, got {sustained}/{reached}"
        );
    }

    // Two families of ONE hardware design, the same size: one EBS figure, and local devices
    // on one of them alone.
    let left = described("c8g.12xlarge", &router, "c8g.12xlarge").await;
    let right = described("c8gd.12xlarge", &router, "c8gd.12xlarge").await;
    assert_eq!(
        bandwidth(&left, "c8g.12xlarge"),
        bandwidth(&right, "c8gd.12xlarge"),
        "one hardware design publishes one EBS figure for both of its families"
    );
    assert!(
        element(&left, "instanceStorageInfo").is_none()
            && element(&right, "instanceStorageInfo").is_some(),
        "only the family whose name carries the local devices reports them"
    );

    // The limit RISES with the size where the volumes have hardware of their own and FALLS
    // where they come out of the pool the type's own local devices eat into.
    let small = described("m7i.12xlarge", &router, "m7i.12xlarge").await;
    let large = described("m7i.24xlarge", &router, "m7i.24xlarge").await;
    assert!(
        attachments(&small, "m7i.12xlarge") < attachments(&large, "m7i.24xlarge"),
        "a dedicated EBS limit rises with the size, got {} then {}",
        attachments(&small, "m7i.12xlarge"),
        attachments(&large, "m7i.24xlarge")
    );
    let small = described("i4i.large", &router, "i4i.large").await;
    let large = described("i4i.16xlarge", &router, "i4i.16xlarge").await;
    assert!(
        attachments(&small, "i4i.large") > attachments(&large, "i4i.16xlarge"),
        "a shared EBS limit falls as the local devices grow, got {} then {}",
        attachments(&small, "i4i.large"),
        attachments(&large, "i4i.16xlarge")
    );

    // A type the table holds no row for is still described, by the derivation over its name.
    let case = "m5.xlarge";
    let body = described(case, &router, case).await;
    let (sustained, reached) = bandwidth(&body, case);
    assert!(
        sustained > 0 && sustained <= reached,
        "case {case:?}: expected a derived EBS figure pair, got {sustained}/{reached}"
    );
}
