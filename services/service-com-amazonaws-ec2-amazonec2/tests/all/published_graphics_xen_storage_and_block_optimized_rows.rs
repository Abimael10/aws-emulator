// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for the seventh generation graphics family,
//! the block storage optimized memory family on the newest AWS Graviton hardware and the two
//! storage optimized families on the Xen hypervisor — driven over the wire the service speaks
//! (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: EVERY HARDWARE MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE
//! TYPE IS THE FIGURE AWS PUBLISHES FOR THAT ONE TYPE, not a function of the size multiplier in
//! its name — and a modelled optional member the type HAS is emitted. The EBS bandwidth and
//! IOPS a type sustains and the figures it reaches, whether it has an EBS-optimized mode at all,
//! how many volumes it holds attached and whether that limit is the volumes' own or shared with
//! its other devices, the EBS cards its volumes are attached to, its local devices and whether
//! that store is encrypted, its network performance, its interface and address limits, its
//! Elastic Fabric Adapters, its accelerators, its processor's sustained frequency, the threads
//! its cores run, the idle timeout its connection tracking applies and the host features it is
//! offered are each a published fact of the type.
//!
//! The tables below span what the rule generalises over: four families that differ in
//! architecture, in hypervisor generation, in having local devices or accelerators at all, and
//! in whether their EBS limit is dedicated or shared; inside each family EVERY size of the
//! ladder is a row, so no one size is the specification. The figures are the ones AWS states
//! for each type on its family's instance-type specification page, in the "Amazon EBS optimized
//! instance types", "Instance volume limits", "Connection tracking timeouts" and "IP addresses
//! per network interface per instance type" tables of the EC2 User Guide.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    assert_published_ebs, described_instance_type, element, emulator, number,
    reported_instance_type_list, reported_instance_type_member, required, MEGABITS_PER_MEGABYTE,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// The published EBS row of one instance type.
#[derive(Debug, Clone, Copy)]
struct EbsRow {
    /// `g7.8xlarge` and the like.
    instance_type: &'static str,
    /// Whether the type has an EBS-optimized mode at all, as `EbsInfo$EbsOptimizedSupport`
    /// reports it.
    optimized: &'static str,
    /// Its four published figures — (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it
    /// reaches) — or `None` for a type with no EBS-optimized mode, which has no performance to
    /// report. A sustained figure that is not a whole number of Mbps is the whole Mbps BELOW
    /// the published one, which is all the integer member can hold
    /// ([`SUSTAINED_HALF_MBPS_ROWS`]).
    figures: Option<(f64, f64, f64, f64)>,
    /// The volumes it holds attached, and whether that limit is the volumes' own or shared with
    /// its interfaces and local devices.
    attachments: (i64, &'static str),
}

/// Every size of the four families, with the EBS row AWS publishes for each.
const EBS_ROWS: [EbsRow; 24] = [
    EbsRow {
        instance_type: "g7.2xlarge",
        optimized: "default",
        figures: Some((4_000.0, 16_000.0, 8_000.0, 32_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "g7.4xlarge",
        optimized: "default",
        figures: Some((8_000.0, 32_000.0, 8_000.0, 32_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "g7.8xlarge",
        optimized: "default",
        figures: Some((16_000.0, 64_000.0, 16_000.0, 64_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "g7.12xlarge",
        optimized: "default",
        figures: Some((20_000.0, 80_000.0, 20_000.0, 80_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "g7.24xlarge",
        optimized: "default",
        figures: Some((40_000.0, 160_000.0, 40_000.0, 160_000.0)),
        attachments: (64, "dedicated"),
    },
    EbsRow {
        instance_type: "g7.48xlarge",
        optimized: "default",
        figures: Some((80_000.0, 320_000.0, 80_000.0, 320_000.0)),
        attachments: (128, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.medium",
        optimized: "default",
        // Half a `large` of the family's 3,125 Mbps per unit: the integer member reports the
        // whole Mbps of 1,562.5 while the throughput one carries the half.
        figures: Some((1_562.0, 7_500.0, 25_000.0, 120_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.large",
        optimized: "default",
        figures: Some((3_125.0, 15_000.0, 25_000.0, 120_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.xlarge",
        optimized: "default",
        figures: Some((6_250.0, 30_000.0, 25_000.0, 120_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.2xlarge",
        optimized: "default",
        figures: Some((12_500.0, 60_000.0, 25_000.0, 120_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.4xlarge",
        optimized: "default",
        figures: Some((25_000.0, 120_000.0, 25_000.0, 120_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.8xlarge",
        optimized: "default",
        figures: Some((50_000.0, 240_000.0, 50_000.0, 240_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.12xlarge",
        optimized: "default",
        figures: Some((75_000.0, 360_000.0, 75_000.0, 360_000.0)),
        attachments: (32, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.16xlarge",
        optimized: "default",
        figures: Some((100_000.0, 480_000.0, 100_000.0, 480_000.0)),
        attachments: (48, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.24xlarge",
        optimized: "default",
        figures: Some((150_000.0, 720_000.0, 150_000.0, 720_000.0)),
        attachments: (64, "dedicated"),
    },
    EbsRow {
        instance_type: "r8gb.48xlarge",
        optimized: "default",
        figures: Some((300_000.0, 1_440_000.0, 300_000.0, 1_440_000.0)),
        attachments: (128, "dedicated"),
    },
    EbsRow {
        instance_type: "h1.2xlarge",
        optimized: "default",
        figures: Some((1_750.0, 10_000.0, 1_750.0, 10_000.0)),
        attachments: (40, "shared"),
    },
    EbsRow {
        instance_type: "h1.4xlarge",
        optimized: "default",
        figures: Some((3_500.0, 20_000.0, 3_500.0, 20_000.0)),
        attachments: (40, "shared"),
    },
    EbsRow {
        instance_type: "h1.8xlarge",
        optimized: "default",
        figures: Some((7_000.0, 40_000.0, 7_000.0, 40_000.0)),
        attachments: (40, "shared"),
    },
    EbsRow {
        instance_type: "h1.16xlarge",
        optimized: "default",
        figures: Some((14_000.0, 80_000.0, 14_000.0, 80_000.0)),
        attachments: (40, "shared"),
    },
    EbsRow {
        instance_type: "i2.xlarge",
        optimized: "supported",
        figures: Some((500.0, 4_000.0, 500.0, 4_000.0)),
        attachments: (40, "shared"),
    },
    EbsRow {
        instance_type: "i2.2xlarge",
        optimized: "supported",
        figures: Some((1_000.0, 8_000.0, 1_000.0, 8_000.0)),
        attachments: (40, "shared"),
    },
    EbsRow {
        instance_type: "i2.4xlarge",
        optimized: "supported",
        figures: Some((2_000.0, 16_000.0, 2_000.0, 16_000.0)),
        attachments: (40, "shared"),
    },
    EbsRow {
        instance_type: "i2.8xlarge",
        // The WIDEST size of this family has no EBS-optimized mode at all, where every size
        // below it has one: no function of the size multiplier yields that.
        optimized: "unsupported",
        figures: None,
        attachments: (40, "shared"),
    },
];

/// The published local instance store of one instance type: its geometry, its device model and
/// whether the store is encrypted.
#[derive(Debug, Clone, Copy)]
struct StoreRow {
    /// `h1.8xlarge` and the like.
    instance_type: &'static str,
    /// Its devices, as (count, GB each) — the total being the product of the two.
    devices: (i64, i64),
    /// `ssd` or `hdd`.
    disk_type: &'static str,
    /// `InstanceStorageInfo$NvmeSupport`, then `$EncryptionSupport`: read apart, because a store
    /// AWS encrypts with a key the host holds reports `required` whether or not its devices are
    /// presented as NVMe ones.
    nvme_and_encryption: (&'static str, &'static str),
}

/// Every size of the three families that carry local devices whose published device this table
/// holds, with the store AWS publishes for each. The memory family has none at any size, which
/// the test below asserts instead.
const STORE_ROWS: [StoreRow; 12] = [
    StoreRow {
        instance_type: "g7.8xlarge",
        devices: (1, 1_900),
        disk_type: "ssd",
        nvme_and_encryption: ("required", "required"),
    },
    StoreRow {
        instance_type: "g7.12xlarge",
        devices: (1, 1_900),
        disk_type: "ssd",
        nvme_and_encryption: ("required", "required"),
    },
    StoreRow {
        instance_type: "g7.24xlarge",
        devices: (1, 3_800),
        disk_type: "ssd",
        nvme_and_encryption: ("required", "required"),
    },
    StoreRow {
        instance_type: "g7.48xlarge",
        devices: (2, 3_800),
        disk_type: "ssd",
        nvme_and_encryption: ("required", "required"),
    },
    StoreRow {
        instance_type: "h1.2xlarge",
        devices: (1, 2_000),
        disk_type: "hdd",
        // Magnetic devices, and an encrypted store all the same.
        nvme_and_encryption: ("unsupported", "required"),
    },
    StoreRow {
        instance_type: "h1.4xlarge",
        devices: (2, 2_000),
        disk_type: "hdd",
        nvme_and_encryption: ("unsupported", "required"),
    },
    StoreRow {
        instance_type: "h1.8xlarge",
        devices: (4, 2_000),
        disk_type: "hdd",
        nvme_and_encryption: ("unsupported", "required"),
    },
    StoreRow {
        instance_type: "h1.16xlarge",
        devices: (8, 2_000),
        disk_type: "hdd",
        nvme_and_encryption: ("unsupported", "required"),
    },
    StoreRow {
        instance_type: "i2.xlarge",
        devices: (1, 800),
        disk_type: "ssd",
        // SATA solid state devices from before the NVMe store, and no encryption offered.
        nvme_and_encryption: ("unsupported", "unsupported"),
    },
    StoreRow {
        instance_type: "i2.2xlarge",
        devices: (2, 800),
        disk_type: "ssd",
        nvme_and_encryption: ("unsupported", "unsupported"),
    },
    StoreRow {
        instance_type: "i2.4xlarge",
        devices: (4, 800),
        disk_type: "ssd",
        nvme_and_encryption: ("unsupported", "unsupported"),
    },
    StoreRow {
        instance_type: "i2.8xlarge",
        devices: (8, 800),
        disk_type: "ssd",
        nvme_and_encryption: ("unsupported", "unsupported"),
    },
];

/// The rows above whose published sustained bandwidth carries a HALF Mbps: the integer
/// `baselineBandwidthInMbps` cannot hold one and reports the whole Mbps below it, while
/// `baselineThroughputInMBps` carries the megabytes of the whole published figure, half
/// included — which is how the published table's Mbps and MB/s columns relate for such a size.
const SUSTAINED_HALF_MBPS_ROWS: [&str; 1] = ["r8gb.medium"];

/// Rule: the EBS-optimized mode, the four figures it carries and the volume limit are published
/// per instance type.
#[tokio::test]
async fn published_ebs_rows_are_served_per_instance_type() {
    let (router, _engine) = emulator();

    for row in EBS_ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let optimized = required(case, &ebs_info, "ebsOptimizedSupport");
        assert_eq!(
            optimized, row.optimized,
            "case {case:?}: expected ebsOptimizedSupport {:?}, got {optimized:?}",
            row.optimized
        );
        match row.figures {
            Some(figures) if SUSTAINED_HALF_MBPS_ROWS.contains(&row.instance_type) => {
                let ebs_optimized_info = required(case, &ebs_info, "ebsOptimizedInfo");
                let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = figures;
                // The half the integer member cannot hold is carried by the throughput one, so
                // the two readings of the one published figure are asserted together.
                let whole_baseline_mbps = baseline_mbps + 0.5;
                for (member, expected) in [
                    ("baselineBandwidthInMbps", baseline_mbps),
                    ("baselineIops", baseline_iops),
                    ("maximumBandwidthInMbps", maximum_mbps),
                    ("maximumIops", maximum_iops),
                    (
                        "baselineThroughputInMBps",
                        whole_baseline_mbps / MEGABITS_PER_MEGABYTE,
                    ),
                    (
                        "maximumThroughputInMBps",
                        maximum_mbps / MEGABITS_PER_MEGABYTE,
                    ),
                ] {
                    let actual = number(case, &ebs_optimized_info, member);
                    assert!(
                        (actual - expected).abs() < 0.001,
                        "case {case:?}: expected <{member}> {expected}, got {actual}"
                    );
                }
            }
            Some(figures) => assert_published_ebs(case, &body, figures),
            None => assert!(
                element(&ebs_info, "ebsOptimizedInfo").is_none(),
                "case {case:?}: a type with no EBS-optimized mode reports no ebsOptimizedInfo, \
                 got {ebs_info}"
            ),
        }
        let (volumes, kind) = row.attachments;
        let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
        assert_eq!(
            reported, volumes,
            "case {case:?}: expected maximumEbsAttachments {volumes}, got {reported}"
        );
        let limit = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            limit, kind,
            "case {case:?}: expected attachmentLimitType {kind:?}, got {limit:?}"
        );
    }
}

/// Rule: a type whose EBS volumes are attached to cards of its own reports those cards, and one
/// whose volumes come out of the host's shared pool reports none at all.
#[tokio::test]
async fn ebs_cards_are_reported_for_the_widest_sizes_that_carry_them() {
    let (router, _engine) = emulator();

    // The widest size of each of the two families with EBS hardware of its own spreads its
    // figures over two cards; the size below it, and a family whose volumes share the host's
    // pool, have no card of their own to report.
    for (instance_type, whole_mbps, whole_iops) in
        [("g7.48xlarge", 80_000_i64, 320_000_i64), ("r8gb.48xlarge", 300_000, 1_440_000)]
    {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let cards = number(case, &ebs_info, "maximumEbsCards") as i64;
        assert_eq!(
            cards, 2,
            "case {case:?}: expected maximumEbsCards 2, got {cards}"
        );
        let card_set = required(case, &ebs_info, "ebsCardSet");
        let items = super::ec2_query::elements(&card_set, "item");
        assert_eq!(
            items.len(),
            2,
            "case {case:?}: expected one ebsCardSet item per card, got {items:?}"
        );
        for (index, item) in items.iter().enumerate() {
            let reported = number(case, item, "ebsCardIndex") as i64;
            assert_eq!(
                reported, index as i64,
                "case {case:?}: expected ebsCardIndex {index}, got {reported}"
            );
            for (member, whole) in [
                ("baselineBandwidthInMbps", whole_mbps),
                ("baselineIops", whole_iops),
                ("maximumBandwidthInMbps", whole_mbps),
                ("maximumIops", whole_iops),
            ] {
                let reported = number(case, item, member) as i64;
                assert_eq!(
                    reported,
                    whole / 2,
                    "case {case:?}: expected card {index} to carry {member} {}, got {reported}",
                    whole / 2
                );
            }
        }
    }

    for instance_type in ["g7.24xlarge", "r8gb.24xlarge", "h1.16xlarge", "i2.4xlarge"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        for member in ["maximumEbsCards", "ebsCardSet"] {
            let reported = element(&ebs_info, member);
            assert!(
                reported.is_none(),
                "case {case:?}: a type with no EBS cards of its own reports no <{member}>, got \
                 {reported:?}"
            );
        }
    }
}

/// Rule: the local devices of a type — how many there are, how big each one is, their model and
/// whether the store is encrypted — are published per type, so the total is the product of a
/// WHOLE number of the family's own device rather than a size split into equal parts.
#[tokio::test]
async fn published_instance_store_geometry_is_served_per_instance_type() {
    let (router, _engine) = emulator();

    for row in STORE_ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let storage = required(case, &body, "instanceStorageInfo");
        let (devices, gb_each) = row.devices;
        let disks = required(case, &storage, "disks");
        let reported = number(case, &disks, "count") as i64;
        assert_eq!(
            reported, devices,
            "case {case:?}: expected {devices} local devices, got {reported}"
        );
        let reported = number(case, &disks, "sizeInGB") as i64;
        assert_eq!(
            reported, gb_each,
            "case {case:?}: expected a {gb_each} GB device, got {reported}"
        );
        let reported = required(case, &disks, "type");
        assert_eq!(
            reported, row.disk_type,
            "case {case:?}: expected a {:?} device, got {reported:?}",
            row.disk_type
        );
        let reported = number(case, &storage, "totalSizeInGB") as i64;
        assert_eq!(
            reported,
            devices * gb_each,
            "case {case:?}: expected totalSizeInGB {}, got {reported}",
            devices * gb_each
        );
        let (nvme, encryption) = row.nvme_and_encryption;
        for (member, expected) in [("nvmeSupport", nvme), ("encryptionSupport", encryption)] {
            let reported = required(case, &storage, member);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected instance store {member} {expected:?}, got {reported:?}"
            );
        }
    }

    // The memory optimized family carries no local device at any size, which it reports as the
    // absence of the whole structure rather than as a store of nothing.
    for instance_type in ["r8gb.medium", "r8gb.4xlarge", "r8gb.48xlarge"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let supported = reported_instance_type_member(case, &body, &["instanceStorageSupported"]);
        assert_eq!(
            supported.as_deref(),
            Some("false"),
            "case {case:?}: a type with no local devices reports instanceStorageSupported false, \
             got {supported:?}"
        );
        assert!(
            reported_instance_type_member(case, &body, &["instanceStorageInfo", "totalSizeInGB"])
                .is_none(),
            "case {case:?}: a type with no local devices reports no instanceStorageInfo, got \
             {body}"
        );
    }
}

/// Rule: the network performance, the interface and address limits, the connection-tracking
/// timeouts and the host features a type is offered are published per type.
#[tokio::test]
async fn published_network_and_host_features_are_served_per_instance_type() {
    let (router, _engine) = emulator();

    // Rows: the published "Network performance" column of all four families, spanning a figure
    // the size sustains, a ceiling it bursts to and the two sizes whose column is TEXT rather
    // than a figure at all — which no arithmetic on the size multiplier produces.
    let network_rows = [
        ("r8gb.medium", "Up to 12.5 Gigabit"),
        ("r8gb.2xlarge", "Up to 15 Gigabit"),
        ("r8gb.8xlarge", "15 Gigabit"),
        ("r8gb.12xlarge", "22.5 Gigabit"),
        ("r8gb.48xlarge", "50 Gigabit"),
        ("h1.2xlarge", "Up to 10 Gigabit"),
        ("h1.8xlarge", "10 Gigabit"),
        ("h1.16xlarge", "25 Gigabit"),
        ("i2.xlarge", "Moderate"),
        ("i2.2xlarge", "High"),
        ("i2.4xlarge", "High"),
        ("i2.8xlarge", "10 Gigabit"),
    ];
    for (instance_type, expected) in network_rows {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "networkPerformance");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected networkPerformance {expected:?}, got {reported:?}"
        );
    }

    // Rows: the established-TCP idle timeout, which the newest hardware generation holds for
    // 350 s where every earlier type holds the 432,000 s the EC2 User Guide publishes, with the
    // two UDP timeouts — the same on every type — read on the same call.
    let timeout_rows = [
        ("r8gb.medium", 350),
        ("r8gb.8xlarge", 350),
        ("r8gb.48xlarge", 350),
        ("g7.8xlarge", 350),
        ("h1.8xlarge", 432_000),
        ("i2.4xlarge", 432_000),
    ];
    for (instance_type, expected) in timeout_rows {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let tracking = required(case, &network_info, "connectionTrackingConfiguration");
        let reported = number(case, &tracking, "defaultTcpEstablishedTimeout") as i64;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected defaultTcpEstablishedTimeout {expected}, got {reported}"
        );
        for (member, shared) in [("defaultUdpTimeout", 30), ("defaultUdpStreamTimeout", 180)] {
            let reported = number(case, &tracking, member) as i64;
            assert_eq!(
                reported, shared,
                "case {case:?}: expected {member} {shared}, got {reported}"
            );
        }
    }

    // Rows: the interfaces a type can attach and the addresses one of them takes — the smallest
    // size of the newest family and the widest size of the dense storage one publish counts of
    // their own, where the sizes between them take the ladder's.
    let interface_rows = [
        ("r8gb.medium", 2, 4),
        ("r8gb.large", 3, 10),
        ("r8gb.xlarge", 4, 15),
        ("h1.16xlarge", 8, 50),
        ("h1.8xlarge", 8, 30),
    ];
    for (instance_type, interfaces, addresses) in interface_rows {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = number(case, &network_info, "maximumNetworkInterfaces") as i64;
        assert_eq!(
            reported, interfaces,
            "case {case:?}: expected maximumNetworkInterfaces {interfaces}, got {reported}"
        );
        let reported = number(case, &network_info, "ipv4AddressesPerInterface") as i64;
        assert_eq!(
            reported, addresses,
            "case {case:?}: expected ipv4AddressesPerInterface {addresses}, got {reported}"
        );
    }

    // The widest size of the graphics family carries a network card per processor socket, and
    // every narrower size of it one card; the first card is the default on every type.
    for (instance_type, cards) in [("g7.48xlarge", 2), ("g7.24xlarge", 1), ("h1.16xlarge", 1)] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = number(case, &network_info, "maximumNetworkCards") as i64;
        assert_eq!(
            reported, cards,
            "case {case:?}: expected maximumNetworkCards {cards}, got {reported}"
        );
        let card_set = required(case, &network_info, "networkCards");
        let indexes = super::ec2_query::elements(&card_set, "networkCardIndex");
        assert_eq!(
            indexes.len(),
            cards as usize,
            "case {case:?}: expected one networkCards item per card, got {indexes:?}"
        );
        let reported = number(case, &network_info, "defaultNetworkCardIndex") as i64;
        assert_eq!(
            reported, 0,
            "case {case:?}: expected defaultNetworkCardIndex 0, got {reported}"
        );
    }

    // Rows: the host features, each a flag a client branches on, over all four families — an
    // Elastic Fabric Adapter from the size that holds a dedicated share of the accelerated
    // host's network up and on the three widest sizes of the memory family, ENA Express on
    // neither of the two newest families, no interface attachment from a secondary network (so
    // no address at all on one), no Traffic Mirroring on any of the four — the two Xen families
    // having no Nitro card to copy a session's packets with — Dedicated Hosts on the older SSD
    // storage family and not on the accelerated one, an enclave from two vCPUs up on the AWS
    // Graviton architecture, and neither reboot migration nor a local PTP hardware clock on the
    // two newest families.
    let feature_rows = [
        ("g7.4xlarge", "efaSupported", "false"),
        ("g7.8xlarge", "efaSupported", "true"),
        ("g7.48xlarge", "efaSupported", "true"),
        ("r8gb.12xlarge", "efaSupported", "false"),
        ("r8gb.16xlarge", "efaSupported", "true"),
        ("r8gb.24xlarge", "efaSupported", "true"),
        ("h1.16xlarge", "efaSupported", "false"),
        ("g7.2xlarge", "enaSrdSupported", "false"),
        ("g7.48xlarge", "enaSrdSupported", "true"),
        ("r8gb.8xlarge", "enaSrdSupported", "false"),
        ("r8gb.48xlarge", "enaSrdSupported", "false"),
        ("g7.12xlarge", "secondaryNetworkSupported", "false"),
        ("r8gb.large", "secondaryNetworkSupported", "false"),
        ("g7.12xlarge", "ipv4AddressesPerSecondaryInterface", "0"),
        ("r8gb.large", "ipv4AddressesPerSecondaryInterface", "0"),
        ("g7.24xlarge", "trafficMirroringSupport", "unsupported"),
        ("r8gb.2xlarge", "trafficMirroringSupport", "unsupported"),
        ("h1.4xlarge", "trafficMirroringSupport", "unsupported"),
        ("i2.2xlarge", "trafficMirroringSupport", "unsupported"),
        ("g7.2xlarge", "dedicatedHostsSupported", "false"),
        ("g7.48xlarge", "dedicatedHostsSupported", "false"),
        ("i2.xlarge", "dedicatedHostsSupported", "true"),
        ("i2.8xlarge", "dedicatedHostsSupported", "true"),
        ("r8gb.medium", "nitroEnclavesSupport", "unsupported"),
        ("r8gb.large", "nitroEnclavesSupport", "supported"),
        ("g7.8xlarge", "rebootMigrationSupport", "unsupported"),
        ("r8gb.24xlarge", "rebootMigrationSupport", "unsupported"),
        ("g7.8xlarge", "phcSupport", "unsupported"),
        ("r8gb.24xlarge", "phcSupport", "unsupported"),
    ];
    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        // A member of networkInfo is read out of that block; the rest are the type's own.
        let scope = if member.starts_with("efa")
            || member.starts_with("ena")
            || member.starts_with("secondaryNetwork")
            || member.starts_with("ipv4")
            || member.starts_with("trafficMirroring")
        {
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

    // A size offered an adapter reports how many it carries, and one offered none reports no
    // efaInfo at all.
    for (instance_type, adapters) in [
        ("g7.8xlarge", Some(1)),
        ("r8gb.16xlarge", Some(1)),
        ("g7.2xlarge", None),
        ("r8gb.8xlarge", None),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = element(&network_info, "efaInfo");
        match adapters {
            Some(count) => {
                let info = reported.unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a type offered an adapter reports its efaInfo, got \
                         {network_info}"
                    )
                });
                let reported = number(case, &info, "maximumEfaInterfaces") as i64;
                assert_eq!(
                    reported, count,
                    "case {case:?}: expected maximumEfaInterfaces {count}, got {reported}"
                );
            }
            None => assert!(
                reported.is_none(),
                "case {case:?}: a type offered no adapter reports no efaInfo, got {reported:?}"
            ),
        }
    }
}

/// Rule: a type's accelerators, its processor's sustained frequency and the threads its cores
/// run are published facts of the type, and a type that carries no accelerator reports none at
/// all.
#[tokio::test]
async fn published_accelerators_and_processors_are_served_per_instance_type() {
    let (router, _engine) = emulator();

    // Rows: the accelerators of every size of the graphics family — one device up to the size
    // that holds a sixth of the host, four on the two middle shares and the host's whole eight
    // on the widest size, each a 32 GiB device whose memory the total is a multiple of.
    for (instance_type, count) in [
        ("g7.2xlarge", 1_i64),
        ("g7.4xlarge", 1),
        ("g7.8xlarge", 1),
        ("g7.12xlarge", 4),
        ("g7.24xlarge", 4),
        ("g7.48xlarge", 8),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let gpu_info = required(case, &body, "gpuInfo");
        let gpus = required(case, &gpu_info, "gpus");
        let reported = number(case, &gpus, "count") as i64;
        assert_eq!(
            reported, count,
            "case {case:?}: expected {count} accelerators, got {reported}"
        );
        let reported = number(case, &gpus, "logicalGpuCount") as i64;
        assert_eq!(
            reported, count,
            "case {case:?}: expected logicalGpuCount {count}, got {reported}"
        );
        let reported = number(case, &gpus, "gpuPartitionSize");
        assert!(
            (reported - 1.0).abs() < 0.001,
            "case {case:?}: an instance that holds whole devices reports gpuPartitionSize 1.0, \
             got {reported}"
        );
        for (member, expected) in [("name", "RTX PRO 4500"), ("manufacturer", "NVIDIA")] {
            let reported = required(case, &gpus, member);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected accelerator {member} {expected:?}, got {reported:?}"
            );
        }
        let memory = required(case, &gpus, "memoryInfo");
        let reported = number(case, &memory, "sizeInMiB") as i64;
        assert_eq!(
            reported, 32_768,
            "case {case:?}: expected a 32768 MiB device, got {reported}"
        );
        let reported = number(case, &gpu_info, "totalGpuMemoryInMiB") as i64;
        assert_eq!(
            reported,
            32_768 * count,
            "case {case:?}: expected totalGpuMemoryInMiB {}, got {reported}",
            32_768 * count
        );
        let workloads =
            reported_instance_type_list(case, &body, &["gpuInfo", "gpus", "item", "workloadSet"]);
        assert_eq!(
            workloads,
            vec!["graphics".to_string(), "ml-ai".to_string()],
            "case {case:?}: expected the published workloads, got {workloads:?}"
        );
    }

    for instance_type in ["r8gb.48xlarge", "h1.16xlarge", "i2.8xlarge"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        assert!(
            element(&body, "gpuInfo").is_none(),
            "case {case:?}: a type with no accelerator reports no gpuInfo, got {body}"
        );
    }

    // Rows: the sustained all-core frequency of each family's processor, in GHz — three
    // different parts, none of them a function of the size.
    for (instance_type, ghz) in [
        ("g7.2xlarge", 3.9),
        ("g7.48xlarge", 3.9),
        ("h1.2xlarge", 2.3),
        ("h1.16xlarge", 2.3),
        ("i2.xlarge", 2.5),
        ("i2.8xlarge", 2.5),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let processor = required(case, &body, "processorInfo");
        let reported = number(case, &processor, "sustainedClockSpeedInGhz");
        assert!(
            (reported - ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {ghz}, got {reported}"
        );
    }

    // The graphics family's cores run ONE thread each, so a size reports its vCPUs as that many
    // cores and the one thread count a launch may ask for; the Xen storage families run two.
    for (instance_type, vcpus, threads) in [
        ("g7.2xlarge", 8_i64, 1_i64),
        ("g7.12xlarge", 48, 1),
        ("g7.48xlarge", 192, 1),
        ("h1.8xlarge", 32, 2),
        ("i2.4xlarge", 16, 2),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let vcpu_info = required(case, &body, "vCpuInfo");
        let reported = number(case, &vcpu_info, "defaultVCpus") as i64;
        assert_eq!(
            reported, vcpus,
            "case {case:?}: expected defaultVCpus {vcpus}, got {reported}"
        );
        let reported = number(case, &vcpu_info, "defaultThreadsPerCore") as i64;
        assert_eq!(
            reported, threads,
            "case {case:?}: expected defaultThreadsPerCore {threads}, got {reported}"
        );
        let reported = number(case, &vcpu_info, "defaultCores") as i64;
        assert_eq!(
            reported,
            vcpus / threads,
            "case {case:?}: expected defaultCores {}, got {reported}",
            vcpus / threads
        );
        let valid = reported_instance_type_list(case, &body, &["vCpuInfo", "validThreadsPerCore"]);
        assert_eq!(
            valid.len(),
            threads as usize,
            "case {case:?}: expected one valid thread count per thread the cores run, got \
             {valid:?}"
        );
    }

    // The widest size of each of the three families whose cores sit on TWO processor sockets
    // takes them one per socket, so its published core counts go up in twos rather than one at
    // a time; a size that sits on one socket publishes every count up to its default.
    for (instance_type, step) in [
        ("h1.16xlarge", 2_usize),
        ("i2.8xlarge", 2),
        ("r8gb.48xlarge", 2),
        ("h1.8xlarge", 1),
        ("i2.4xlarge", 1),
        ("g7.24xlarge", 1),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let counts = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"]);
        let first = counts
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: a type publishes its core counts, got {body}"));
        assert_eq!(
            first,
            &step.to_string(),
            "case {case:?}: expected the smallest published core count {step}, got {counts:?}"
        );
        let cores = number(case, &required(case, &body, "vCpuInfo"), "defaultCores") as usize;
        assert_eq!(
            counts.len(),
            cores / step,
            "case {case:?}: expected one count per step up to {cores} cores, got {counts:?}"
        );
    }

    // The size that holds the whole accelerated host takes its cores ALL at once, so the one
    // count it can be configured with is its default.
    let case = "g7.48xlarge";
    let body = described(case, &router, case).await;
    let counts = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"]);
    assert_eq!(
        counts,
        vec!["192".to_string()],
        "case {case:?}: expected the one published core count, got {counts:?}"
    );
}

/// Rule: the virtualization types, root device types and placement group strategies a type is
/// offered are published per type — a previous generation family whose AMIs are HVM ones alone
/// can still boot from an instance store, and a previous generation family can still be offered
/// a cluster placement group.
#[tokio::test]
async fn published_image_and_placement_support_is_served_per_instance_type() {
    let (router, _engine) = emulator();

    for instance_type in ["i2.xlarge", "i2.2xlarge", "i2.4xlarge", "i2.8xlarge"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let virtualization =
            reported_instance_type_list(case, &body, &["supportedVirtualizationTypes"]);
        assert_eq!(
            virtualization,
            vec!["hvm".to_string()],
            "case {case:?}: expected the HVM virtualization type alone, got {virtualization:?}"
        );
        let roots = reported_instance_type_list(case, &body, &["supportedRootDeviceTypes"]);
        assert_eq!(
            roots,
            vec!["ebs".to_string(), "instance-store".to_string()],
            "case {case:?}: expected both published root device types, got {roots:?}"
        );
        let strategies =
            reported_instance_type_list(case, &body, &["placementGroupInfo", "supportedStrategies"]);
        assert_eq!(
            strategies,
            vec![
                "cluster".to_string(),
                "partition".to_string(),
                "spread".to_string()
            ],
            "case {case:?}: expected every published placement strategy, got {strategies:?}"
        );
        let encryption = required(case, &required(case, &body, "ebsInfo"), "encryptionSupport");
        assert_eq!(
            encryption, "supported",
            "case {case:?}: expected Amazon EBS encryption to be supported, got {encryption:?}"
        );
    }
}

/// Rule: the published figures are facts of the TYPE, so two families at one size differ and one
/// family's sizes do not scale by the size multiplier.
#[tokio::test]
async fn published_rows_are_not_a_function_of_the_size_or_the_family_letter() {
    let (router, _engine) = emulator();

    // One size, two families: the block storage optimized memory family and the plain memory
    // family of the SAME generation publish different EBS figures, so neither the size nor the
    // generation produced them.
    let left = described("r8gb.8xlarge", &router, "r8gb.8xlarge").await;
    let right = described("r8g.8xlarge", &router, "r8g.8xlarge").await;
    let baseline = |case: &str, body: &str| {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        number(case, &performance, "baselineBandwidthInMbps") as i64
    };
    assert_ne!(
        baseline("r8gb.8xlarge", &left),
        baseline("r8g.8xlarge", &right),
        "two families of one size and one generation publish different EBS figures"
    );

    // One size, two graphics generations: the local store of the seventh generation is ONE
    // device of its own size where the sixth generation's size of the same width carries two.
    for (left, right) in [("g7.8xlarge", "g6.8xlarge")] {
        let left_body = described(left, &router, left).await;
        let right_body = described(right, &router, right).await;
        let devices = |case: &str, body: &str| {
            let storage = required(case, body, "instanceStorageInfo");
            let disks = required(case, &storage, "disks");
            number(case, &disks, "count") as i64
        };
        assert_ne!(
            devices(left, &left_body),
            devices(right, &right_body),
            "two generations of one family letter at one size publish different local stores"
        );
    }

    // One family, two sizes: the dense storage family's widest size does not publish twice the
    // EBS bandwidth of the size half its width's double, because a published figure is a row of
    // a table rather than a multiple — and the older storage family's widest size has no
    // EBS-optimized mode at all while every smaller size has one.
    let narrow = described("i2.4xlarge", &router, "i2.4xlarge").await;
    let wide = described("i2.8xlarge", &router, "i2.8xlarge").await;
    let narrow_info = required("i2.4xlarge", &narrow, "ebsInfo");
    let wide_info = required("i2.8xlarge", &wide, "ebsInfo");
    assert!(
        element(&narrow_info, "ebsOptimizedInfo").is_some()
            && element(&wide_info, "ebsOptimizedInfo").is_none(),
        "the narrower size publishes an EBS-optimized performance its widest size does not: got \
         {narrow_info} then {wide_info}"
    );
}
