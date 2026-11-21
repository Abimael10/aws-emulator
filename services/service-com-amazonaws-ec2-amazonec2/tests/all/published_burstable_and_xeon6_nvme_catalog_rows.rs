// SPDX-License-Identifier: CC0-1.0

//! The published rows of the burstable families and of the eighth generation Intel NVMe
//! compute family, driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: EVERY MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE TYPE'S
//! HARDWARE IS THE FIGURE AWS PUBLISHES FOR THAT ONE TYPE, taken from the type's own row of
//! the catalog, and no function of the size multiplier in its name stands in for it. That
//! covers the EBS bandwidth and IOPS it sustains and the ones it reaches
//! (`EbsInfo$EbsOptimizedInfo`), how many volumes it holds attached and whether that limit is
//! the volumes' own (`$MaximumEbsAttachments` / `$AttachmentLimitType`), its local devices
//! (`InstanceStorageInfo$Disks` / `$TotalSizeInGB`), its network performance, the bandwidth
//! weightings it accepts and the idle timeout its connection tracking applies
//! (`NetworkInfo$NetworkPerformance` / `$BandwidthWeightings` /
//! `DefaultConnectionTrackingConfiguration$DefaultTcpEstablishedTimeout`), the private
//! addresses one of its interfaces takes (`$Ipv4AddressesPerInterface`), its processor's
//! sustained frequency (`ProcessorInfo$SustainedClockSpeedInGhz`), the core counts it can be
//! configured with (`VCpuInfo$ValidCores`) and the host features it is offered (hibernation,
//! reboot migration, a PTP hardware clock).
//!
//! The tables span what the rule generalises over rather than one report's types: two
//! BURSTABLE families of different hardware — one whose volumes come out of the pool shared
//! with its interfaces and one whose volumes have a limit of their own — every size of each,
//! so the rows differ in EBS baseline, in IOPS, in the ceiling they burst to and in the
//! addresses one interface takes; and a compute family whose host is twice the width of the
//! generation before it, from its smallest size through the size at which its bandwidth stops
//! bursting, the sizes whose local device COUNT grows, the size that holds the whole host and
//! both bare metal sizes, so the rows differ in device size, device count, network
//! performance, attachment limit and limit KIND.
//!
//! The figures are the ones AWS states per type: the family's own instance-type specification
//! page (vCPUs, network performance, the local devices), the "Amazon EBS optimized instance
//! types" table (the four EBS figures), "Instance volume limits" (the attachment limit and its
//! kind) and the "IP addresses per network interface per instance type" table.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{assert_published_ebs, described_instance_type, elements, number, required};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The body `DescribeInstanceTypes` answers for exactly one instance type, through the hop
/// every suite that reads the catalog shares.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// How close a reading has to be to the figure a published row states for the two to be the
/// same figure: these members are whole numbers and exact decimals, so this is slack for the
/// text-to-float round trip rather than a tolerance on the value itself.
const SAME_FIGURE: f64 = 0.001;

/// The published row of one size of a burstable family.
#[derive(Debug, Clone, Copy)]
struct BurstableRow {
    /// `t4g.micro` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (f64, f64, f64, f64),
    /// How many Amazon EBS volumes it holds attached, and whether that limit is shared with
    /// its interfaces and local devices or is the volumes' own.
    ebs_attachments: (f64, &'static str),
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// The private IPv4 addresses one of its interfaces takes.
    ipv4_addresses_per_interface: f64,
    /// The idle timeout, in seconds, its connection tracking applies to an established TCP
    /// connection.
    tcp_established_timeout: f64,
}

/// Every size of the two burstable families, with the figures AWS publishes for each.
///
/// Two hardware designs, so nothing but the published row separates the two halves of the
/// table: the Graviton2 family's volumes come out of the pool it shares with its interfaces
/// and its baseline bursts to one of two ceilings depending on the size, where the Intel Xeon 6
/// family's volumes have a limit of their OWN — the same 32 at every size — its ceiling is one
/// figure for the whole family, its IOPS ladder is half again as high at the same bandwidth,
/// and its connection tracking holds an established connection for a fraction as long. The
/// `large` size of a T ladder takes twelve private addresses per interface where the generic
/// ladder's `large` step takes ten, and the sizes below `small` take two.
const BURSTABLE_ROWS: [BurstableRow; 11] = [
    BurstableRow {
        instance_type: "t4g.nano",
        ebs: (43.0, 250.0, 2_085.0, 11_800.0),
        ebs_attachments: (27.0, "shared"),
        network_performance: "Up to 5 Gigabit",
        ipv4_addresses_per_interface: 2.0,
        tcp_established_timeout: 432_000.0,
    },
    BurstableRow {
        instance_type: "t4g.micro",
        ebs: (87.0, 500.0, 2_085.0, 11_800.0),
        ebs_attachments: (27.0, "shared"),
        network_performance: "Up to 5 Gigabit",
        ipv4_addresses_per_interface: 2.0,
        tcp_established_timeout: 432_000.0,
    },
    BurstableRow {
        instance_type: "t4g.small",
        ebs: (174.0, 1_000.0, 2_085.0, 11_800.0),
        ebs_attachments: (27.0, "shared"),
        network_performance: "Up to 5 Gigabit",
        ipv4_addresses_per_interface: 4.0,
        tcp_established_timeout: 432_000.0,
    },
    BurstableRow {
        instance_type: "t4g.medium",
        ebs: (347.0, 2_000.0, 2_085.0, 11_800.0),
        ebs_attachments: (27.0, "shared"),
        network_performance: "Up to 5 Gigabit",
        ipv4_addresses_per_interface: 6.0,
        tcp_established_timeout: 432_000.0,
    },
    BurstableRow {
        instance_type: "t4g.large",
        ebs: (695.0, 4_000.0, 2_780.0, 15_700.0),
        ebs_attachments: (27.0, "shared"),
        network_performance: "Up to 5 Gigabit",
        ipv4_addresses_per_interface: 12.0,
        tcp_established_timeout: 432_000.0,
    },
    BurstableRow {
        instance_type: "t4g.xlarge",
        ebs: (1_390.0, 6_000.0, 2_780.0, 15_700.0),
        ebs_attachments: (27.0, "shared"),
        network_performance: "Up to 5 Gigabit",
        ipv4_addresses_per_interface: 15.0,
        tcp_established_timeout: 432_000.0,
    },
    BurstableRow {
        instance_type: "t4g.2xlarge",
        ebs: (2_780.0, 11_800.0, 2_780.0, 15_700.0),
        ebs_attachments: (27.0, "shared"),
        network_performance: "Up to 5 Gigabit",
        ipv4_addresses_per_interface: 15.0,
        tcp_established_timeout: 432_000.0,
    },
    BurstableRow {
        instance_type: "t8i.nano",
        ebs: (43.0, 375.0, 5_000.0, 20_000.0),
        ebs_attachments: (32.0, "dedicated"),
        network_performance: "Up to 12.5 Gigabit",
        ipv4_addresses_per_interface: 2.0,
        tcp_established_timeout: 350.0,
    },
    BurstableRow {
        instance_type: "t8i.micro",
        ebs: (87.0, 750.0, 5_000.0, 20_000.0),
        ebs_attachments: (32.0, "dedicated"),
        network_performance: "Up to 12.5 Gigabit",
        ipv4_addresses_per_interface: 2.0,
        tcp_established_timeout: 350.0,
    },
    BurstableRow {
        instance_type: "t8i.small",
        ebs: (174.0, 1_500.0, 5_000.0, 20_000.0),
        ebs_attachments: (32.0, "dedicated"),
        network_performance: "Up to 12.5 Gigabit",
        ipv4_addresses_per_interface: 4.0,
        tcp_established_timeout: 350.0,
    },
    BurstableRow {
        instance_type: "t8i.medium",
        ebs: (347.0, 3_000.0, 5_000.0, 20_000.0),
        ebs_attachments: (32.0, "dedicated"),
        network_performance: "Up to 12.5 Gigabit",
        ipv4_addresses_per_interface: 6.0,
        tcp_established_timeout: 350.0,
    },
];

/// The published row of one size of the eighth generation Intel NVMe compute family.
#[derive(Debug, Clone, Copy)]
struct NvmeComputeRow {
    /// `c8id.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (f64, f64, f64, f64),
    /// How many Amazon EBS volumes it holds attached, and whether that limit is the volumes'
    /// own or the pool its host shares between every device.
    ebs_attachments: (f64, &'static str),
    /// Its local devices as (count, GB each).
    instance_store: (f64, f64),
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
}

/// The sizes of the eighth generation Intel NVMe compute family this table spans.
///
/// One family of ONE hardware design, so nothing but the published row separates two rows: the
/// EBS baseline bursts to the family's ceiling on the four smallest sizes and is dedicated from
/// the `8xlarge` up, where it grows with the size until the bandwidth reaches the host's own
/// 80,000 Mbps and the IOPS keep doubling past it; the local store is one device of the share
/// of the host's storage the size holds until that share passes one device of this hardware,
/// and whole or halved devices of that size above it; the attachment limit grows in four steps
/// and is the volumes' own at every virtualized size, where each bare metal size reports
/// instead the one pool its host shares between every device.
const NVME_COMPUTE_ROWS: [NvmeComputeRow; 9] = [
    NvmeComputeRow {
        instance_type: "c8id.large",
        ebs: (650.0, 3_600.0, 10_000.0, 40_000.0),
        ebs_attachments: (32.0, "dedicated"),
        instance_store: (1.0, 118.0),
        network_performance: "Up to 12.5 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.2xlarge",
        ebs: (2_500.0, 12_000.0, 10_000.0, 40_000.0),
        ebs_attachments: (32.0, "dedicated"),
        instance_store: (1.0, 474.0),
        network_performance: "Up to 15 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.8xlarge",
        ebs: (10_000.0, 40_000.0, 10_000.0, 40_000.0),
        ebs_attachments: (32.0, "dedicated"),
        instance_store: (1.0, 1_900.0),
        network_performance: "15 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.12xlarge",
        ebs: (15_000.0, 60_000.0, 15_000.0, 60_000.0),
        ebs_attachments: (32.0, "dedicated"),
        instance_store: (1.0, 2_850.0),
        network_performance: "22.5 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.16xlarge",
        ebs: (20_000.0, 80_000.0, 20_000.0, 80_000.0),
        ebs_attachments: (48.0, "dedicated"),
        instance_store: (1.0, 3_800.0),
        network_performance: "30 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.24xlarge",
        ebs: (30_000.0, 120_000.0, 30_000.0, 120_000.0),
        ebs_attachments: (64.0, "dedicated"),
        instance_store: (2.0, 2_850.0),
        network_performance: "40 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.48xlarge",
        ebs: (60_000.0, 240_000.0, 60_000.0, 240_000.0),
        ebs_attachments: (128.0, "dedicated"),
        instance_store: (3.0, 3_800.0),
        network_performance: "75 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.96xlarge",
        ebs: (80_000.0, 480_000.0, 80_000.0, 480_000.0),
        ebs_attachments: (128.0, "dedicated"),
        instance_store: (6.0, 3_800.0),
        network_performance: "100 Gigabit",
    },
    NvmeComputeRow {
        instance_type: "c8id.metal-96xl",
        ebs: (80_000.0, 480_000.0, 80_000.0, 480_000.0),
        ebs_attachments: (79.0, "shared"),
        instance_store: (6.0, 3_800.0),
        network_performance: "100 Gigabit",
    },
];

/// Rule: every size of a burstable family serves its own published EBS figures, attachment
/// limit, network performance, per-interface address count and connection-tracking timeout.
#[tokio::test]
async fn burstable_rows_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in BURSTABLE_ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        assert_published_ebs(case, &body, row.ebs);

        let ebs_info = required(case, &body, "ebsInfo");
        let (attachments, limit_kind) = row.ebs_attachments;
        let actual = number(case, &ebs_info, "maximumEbsAttachments");
        assert!(
            (actual - attachments).abs() < SAME_FIGURE,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {actual}"
        );
        let actual = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            actual, limit_kind,
            "case {case:?}: expected attachmentLimitType {limit_kind:?}, got {actual:?}"
        );

        let network_info = required(case, &body, "networkInfo");
        let actual = required(case, &network_info, "networkPerformance");
        assert_eq!(
            actual, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {actual:?}",
            row.network_performance
        );
        for member in ["ipv4AddressesPerInterface", "ipv6AddressesPerInterface"] {
            let actual = number(case, &network_info, member);
            assert!(
                (actual - row.ipv4_addresses_per_interface).abs() < SAME_FIGURE,
                "case {case:?}: expected {member} {}, got {actual}",
                row.ipv4_addresses_per_interface
            );
        }
        let tracking = required(case, &network_info, "connectionTrackingConfiguration");
        let actual = number(case, &tracking, "defaultTcpEstablishedTimeout");
        assert!(
            (actual - row.tcp_established_timeout).abs() < SAME_FIGURE,
            "case {case:?}: expected defaultTcpEstablishedTimeout {}, got {actual}",
            row.tcp_established_timeout
        );

        // Every size of a burstable family runs its vCPUs at a baseline rate and spends the
        // credits they earn to run above it, which is what the burst model names.
        let actual = required(case, &body, "cpuBurstModel");
        assert_eq!(
            actual, "credit",
            "case {case:?}: expected cpuBurstModel \"credit\", got {actual:?}"
        );
    }
}

/// Rule: every size of the eighth generation Intel NVMe compute family serves its own published
/// EBS figures, attachment limit and limit kind, local devices and network performance.
#[tokio::test]
async fn nvme_compute_rows_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in NVME_COMPUTE_ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        assert_published_ebs(case, &body, row.ebs);

        let ebs_info = required(case, &body, "ebsInfo");
        let (attachments, limit_kind) = row.ebs_attachments;
        let actual = number(case, &ebs_info, "maximumEbsAttachments");
        assert!(
            (actual - attachments).abs() < SAME_FIGURE,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {actual}"
        );
        let actual = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            actual, limit_kind,
            "case {case:?}: expected attachmentLimitType {limit_kind:?}, got {actual:?}"
        );

        let storage = required(case, &body, "instanceStorageInfo");
        let disks = required(case, &storage, "disks");
        let (devices, gb_each) = row.instance_store;
        for (member, expected, scope) in [
            ("count", devices, &disks),
            ("sizeInGB", gb_each, &disks),
            ("totalSizeInGB", devices * gb_each, &storage),
        ] {
            let actual = number(case, scope, member);
            assert!(
                (actual - expected).abs() < SAME_FIGURE,
                "case {case:?}: expected {member} {expected}, got {actual}"
            );
        }

        let network_info = required(case, &body, "networkInfo");
        let actual = required(case, &network_info, "networkPerformance");
        assert_eq!(
            actual, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {actual:?}",
            row.network_performance
        );
        // The family's network and EBS bandwidth is configurable, so every size publishes the
        // weightings a launch may ask for, in the order AWS lists them.
        let weightings = required(case, &network_info, "bandwidthWeightings");
        let actual = elements(&weightings, "item");
        assert_eq!(
            actual,
            vec!["default", "ebs-1", "vpc-1"],
            "case {case:?}: expected the configurable bandwidth weightings, got {actual:?}"
        );
    }
}

/// Rule: the host features, the adapter features and the processor of a published row are the
/// type's own too — the local data of an instance store cannot be carried to another host, so no
/// size of the NVMe compute family is offered reboot migration, neither family is offered
/// hibernation on the hardware that reaches it, and a burstable size carries less dedicated
/// network bandwidth than ENA Express needs.
#[tokio::test]
async fn published_rows_serve_the_host_features_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: one claim per member, read on a virtualized size and on a bare metal one of the
    // compute family, and on both ends of each burstable ladder. A member of `networkInfo` is
    // read out of that block; the rest are the reported type's own.
    let feature_rows: [(&str, &str, &str); 22] = [
        ("c8id.large", "rebootMigrationSupport", "unsupported"),
        ("c8id.24xlarge", "rebootMigrationSupport", "unsupported"),
        ("c8id.metal-96xl", "rebootMigrationSupport", "unsupported"),
        ("c8id.large", "phcSupport", "supported"),
        ("c8id.metal-96xl", "phcSupport", "supported"),
        ("c8id.large", "hibernationSupported", "false"),
        ("c8id.12xlarge", "hibernationSupported", "false"),
        ("t4g.nano", "burstablePerformanceSupported", "true"),
        ("t8i.medium", "burstablePerformanceSupported", "true"),
        ("c8id.96xlarge", "cpuBurstModel", "none"),
        // The Elastic Fabric Adapter reaches the widest sizes of the compute family and both
        // bare metal ones, and no size below them.
        ("c8id.24xlarge", "efaSupported", "false"),
        ("c8id.48xlarge", "efaSupported", "true"),
        ("c8id.metal-96xl", "efaSupported", "true"),
        // A bare metal size of this generation is offered a trusted platform module, which is
        // what its UEFI boot mode is needed for; a virtualized size of the same family has the
        // one its hypervisor presents.
        ("c8id.metal-48xl", "nitroTpmSupport", "supported"),
        ("c8id.metal-96xl", "nitroTpmSupport", "supported"),
        // ENA Express is offered from the size whose dedicated network share its transport
        // needs, and on neither burstable family.
        ("c8id.2xlarge", "enaSrdSupported", "false"),
        ("c8id.12xlarge", "enaSrdSupported", "true"),
        ("t8i.small", "enaSrdSupported", "false"),
        ("t4g.large", "enaSrdSupported", "false"),
        // Neither family takes an interface attachment from a secondary network, so no private
        // address at all is available on one, and the packets of a session cannot be copied
        // under the guest of the generation that publishes its own answer.
        ("t8i.nano", "secondaryNetworkSupported", "false"),
        ("t8i.nano", "ipv4AddressesPerSecondaryInterface", "0"),
        ("c8id.8xlarge", "trafficMirroringSupport", "unsupported"),
    ];

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let scope = if matches!(
            member,
            "efaSupported"
                | "enaSrdSupported"
                | "secondaryNetworkSupported"
                | "ipv4AddressesPerSecondaryInterface"
                | "trafficMirroringSupport"
        ) {
            required(&case, &body, "networkInfo")
        } else {
            body
        };
        let actual = required(&case, &scope, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?}, got {actual:?}"
        );
    }

    // The processor's sustained frequency is published per family, not derived from the
    // generation digit in the name: the Graviton2 burstable family and the two Intel Xeon 6
    // families disagree, the burstable and the compute family built on the SAME processor
    // agree, and the figure is the same at every size of one family.
    for (instance_type, expected) in [
        ("t4g.nano", 2.5),
        ("t4g.2xlarge", 2.5),
        ("t8i.small", 3.9),
        ("c8id.large", 3.9),
        ("c8id.metal-96xl", 3.9),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let processor = required(case, &body, "processorInfo");
        let actual = number(case, &processor, "sustainedClockSpeedInGhz");
        assert!(
            (actual - expected).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {expected}, got {actual}"
        );
    }
    // The boot modes a bare metal size of this generation publishes are its own: the UEFI mode
    // alone, where a virtualized size of the same family is offered both.
    for (instance_type, expected) in [
        ("c8id.metal-48xl", vec!["uefi"]),
        ("c8id.metal-96xl", vec!["uefi"]),
        ("c8id.48xlarge", vec!["legacy-bios", "uefi"]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let modes = required(case, &body, "supportedBootModes");
        let actual = elements(&modes, "item");
        assert_eq!(
            actual, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {actual:?}"
        );
    }
}

/// Rule, for the one member whose published list is the type's own rather than every count up to
/// its default: the core counts a size can be configured with
/// (`VCpuInfo$ValidCores`) are the ones its own row publishes — the sizes of the NVMe compute
/// family that are wide enough take their cores ALL at once, where the narrower sizes of the same
/// family take any count up to their default.
#[tokio::test]
async fn published_rows_serve_the_core_counts_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: the three sizes whose published list is one count — its own default — and, as the
    // other half of the rule, a narrower size of the same family whose list is every count.
    for (instance_type, expected) in [
        ("c8id.24xlarge", vec!["48".to_string()]),
        ("c8id.48xlarge", vec!["96".to_string()]),
        ("c8id.96xlarge", vec!["192".to_string()]),
        (
            "c8id.12xlarge",
            (1..=24).map(|cores| cores.to_string()).collect(),
        ),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let v_cpu_info = required(case, &body, "vCpuInfo");
        let valid_cores = required(case, &v_cpu_info, "validCores");
        let actual = elements(&valid_cores, "item");
        assert_eq!(
            actual, expected,
            "case {case:?}: expected validCores {expected:?}, got {actual:?}"
        );
    }
}

/// Rule, as an invariant over every row: a type sustains no more than it reaches, its local
/// devices sum to the total it publishes, and the published figures do NOT follow the size
/// multiplier — two sizes of one family share a figure where the multiplier differs, and two
/// families of one size disagree where the multiplier is equal.
#[tokio::test]
async fn the_published_figures_are_bounded_and_do_not_follow_the_size_multiplier() {
    let (router, _engine) = super::ec2_query::emulator();

    let bandwidth = |body: &str, case: &str| -> (f64, f64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps"),
            number(case, &performance, "maximumBandwidthInMbps"),
        )
    };

    let every_type = BURSTABLE_ROWS
        .iter()
        .map(|row| row.instance_type)
        .chain(NVME_COMPUTE_ROWS.iter().map(|row| row.instance_type));
    for instance_type in every_type {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let (sustained, reached) = bandwidth(&body, case);
        assert!(
            sustained > 0.0 && sustained <= reached,
            "case {case:?}: a type sustains at most what it reaches, got {sustained}/{reached}"
        );
    }

    // Two sizes of one burstable family, three steps of the ladder apart, bursting to ONE
    // ceiling: the multiplier cannot be what produced either.
    let left = described("t4g.nano", &router, "t4g.nano").await;
    let right = described("t4g.medium", &router, "t4g.medium").await;
    assert_eq!(
        bandwidth(&left, "t4g.nano").1,
        bandwidth(&right, "t4g.medium").1,
        "the sizes below the t4g large step burst to one published ceiling"
    );

    // ONE size, two burstable families: the same multiplier, different hardware, so the
    // figures differ — the sustained bandwidth agrees while the IOPS and the ceiling do not.
    let graviton = described("t4g.small", &router, "t4g.small").await;
    let intel = described("t8i.small", &router, "t8i.small").await;
    assert_eq!(
        bandwidth(&graviton, "t4g.small").0,
        bandwidth(&intel, "t8i.small").0,
        "the two burstable families publish one sustained bandwidth at the small size"
    );
    assert_ne!(
        bandwidth(&graviton, "t4g.small").1,
        bandwidth(&intel, "t8i.small").1,
        "the two burstable families burst to different published ceilings"
    );
}
