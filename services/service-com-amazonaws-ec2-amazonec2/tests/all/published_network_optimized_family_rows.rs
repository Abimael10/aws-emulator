// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for the eighth generation Intel network
//! optimized compute family and for the Ice Lake high-memory family with local NVMe devices
//! — C8in and X2idn — driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: EVERY HARDWARE MEMBER `DescribeInstanceTypes` REPORTS IS THE FIGURE
//! AWS PUBLISHES FOR THAT ONE INSTANCE TYPE, not a function of the size multiplier in its
//! name — and the members that report ONE published figure through different readings (the
//! network performance text and a single card's bandwidth in Gbps, the EBS bandwidth in Mbps
//! and the throughput in MB/s) cannot disagree about it.
//!
//! The tables below span what the rule generalises over. The two families differ in processor
//! generation, in whether they carry local devices at all, in whether their EBS volume limit
//! is the volumes' own or a share of the pool they take with every other device, in how long
//! their connection tracking holds an established TCP connection, and in the host features
//! they are offered; inside each family every published size is a row — the ones that burst
//! above a baseline and the ones with dedicated bandwidth, the ones offered an Elastic Fabric
//! Adapter and the ones that are not, the virtualized ones and the bare metal ones — so no
//! one size is the specification. The figures are the ones AWS states for each type on its
//! family's instance-type specification page, in the "Amazon EBS optimized instance types"
//! and "Instance volume limits" tables and in the "IP addresses per network interface per
//! instance type" table of the EC2 User Guide.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    described_instance_type, owned, reported_instance_type_list, reported_instance_type_member,
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

/// The member at `path` of the one reported instance type, or a panic naming the case when
/// the type reports none.
fn member(case: &str, body: &str, path: &[&str]) -> String {
    reported_instance_type_member(case, body, path).unwrap_or_else(|| {
        panic!(
            "case {case:?}: no <{}> reported, got {body}",
            path.join("/")
        )
    })
}

/// The same member as a number.
fn figure(case: &str, body: &str, path: &[&str]) -> f64 {
    let text = member(case, body, path);
    text.parse().unwrap_or_else(|_| {
        panic!(
            "case {case:?}: <{}> must be a number, got {text:?}",
            path.join("/")
        )
    })
}

/// The storage and EBS row of one instance type: the figures a client sizes its volumes and
/// its devices from.
#[derive(Debug, Clone, Copy)]
struct StorageRow {
    /// `c8in.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// The EBS volumes it holds attached, and whether that limit is shared with its
    /// interfaces and local devices or is the volumes' own. The figure is `None` for a size
    /// whose published limit the catalog does not carry yet, which still publishes the KIND
    /// of limit its volumes have.
    ebs_attachments: (Option<i64>, &'static str),
    /// The EBS cards its volumes are attached to, where it carries hardware of its own for
    /// them, and `None` where its volumes come out of the host's shared pool.
    ebs_cards: Option<usize>,
    /// Its local devices as (count, GB each), or `None` where it carries none at all.
    instance_store: Option<(i64, i64)>,
}

/// Every published size of the two families, with the storage figures AWS publishes for each.
///
/// C8in: 8th generation Intel compute built for the network, no local devices, EBS figures
/// that burst above a baseline below the `8xlarge` and are dedicated from it up, a volume
/// limit of the volumes' own that grows with the size, and two EBS cards on the width that
/// holds the whole host — while a bare metal size shares its host's pool with every other
/// device.
/// X2idn: Ice Lake memory optimized with local NVMe devices, EBS bandwidth dedicated at every
/// published size with the family's IOPS ceiling reached from the `24xlarge` up, and a volume
/// limit that is what the shared attachment pool leaves once its devices are counted.
const STORAGE_ROWS: [StorageRow; 17] = [
    StorageRow {
        instance_type: "c8in.large",
        ebs: (650, 3_600, 10_000, 40_000),
        ebs_attachments: (Some(32), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.xlarge",
        ebs: (1_250, 6_000, 10_000, 40_000),
        ebs_attachments: (Some(32), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.2xlarge",
        ebs: (2_500, 12_000, 10_000, 40_000),
        ebs_attachments: (Some(32), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.4xlarge",
        ebs: (5_000, 20_000, 10_000, 40_000),
        ebs_attachments: (Some(32), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.8xlarge",
        ebs: (10_000, 40_000, 10_000, 40_000),
        ebs_attachments: (Some(32), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.12xlarge",
        ebs: (15_000, 60_000, 15_000, 60_000),
        ebs_attachments: (Some(32), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.16xlarge",
        ebs: (20_000, 80_000, 20_000, 80_000),
        ebs_attachments: (Some(48), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.24xlarge",
        ebs: (30_000, 120_000, 30_000, 120_000),
        ebs_attachments: (Some(64), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.32xlarge",
        ebs: (40_000, 160_000, 40_000, 160_000),
        ebs_attachments: (None, "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.48xlarge",
        ebs: (60_000, 240_000, 60_000, 240_000),
        ebs_attachments: (Some(128), "dedicated"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.96xlarge",
        ebs: (120_000, 480_000, 120_000, 480_000),
        ebs_attachments: (Some(128), "dedicated"),
        ebs_cards: Some(2),
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.metal-48xl",
        ebs: (60_000, 240_000, 60_000, 240_000),
        ebs_attachments: (Some(31), "shared"),
        ebs_cards: None,
        instance_store: None,
    },
    StorageRow {
        instance_type: "c8in.metal-96xl",
        ebs: (120_000, 480_000, 120_000, 480_000),
        ebs_attachments: (Some(31), "shared"),
        ebs_cards: Some(2),
        instance_store: None,
    },
    StorageRow {
        instance_type: "x2idn.16xlarge",
        ebs: (40_000, 173_333, 40_000, 173_333),
        ebs_attachments: (Some(26), "shared"),
        ebs_cards: None,
        instance_store: Some((1, 1_900)),
    },
    StorageRow {
        instance_type: "x2idn.24xlarge",
        ebs: (60_000, 260_000, 60_000, 260_000),
        ebs_attachments: (Some(25), "shared"),
        ebs_cards: None,
        instance_store: Some((2, 1_425)),
    },
    StorageRow {
        instance_type: "x2idn.32xlarge",
        ebs: (80_000, 260_000, 80_000, 260_000),
        ebs_attachments: (Some(25), "shared"),
        ebs_cards: None,
        instance_store: Some((2, 1_900)),
    },
    StorageRow {
        instance_type: "x2idn.metal",
        ebs: (80_000, 260_000, 80_000, 260_000),
        ebs_attachments: (Some(31), "shared"),
        ebs_cards: None,
        instance_store: Some((2, 1_900)),
    },
];

/// Rule: the EBS and instance store members a type reports are the figures AWS publishes for
/// THAT type, whatever its size multiplier would give.
#[tokio::test]
async fn published_storage_rows_are_the_figures_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in STORAGE_ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = row.ebs;
        for (name, expected) in [
            ("baselineBandwidthInMbps", baseline_mbps),
            ("baselineIops", baseline_iops),
            ("maximumBandwidthInMbps", maximum_mbps),
            ("maximumIops", maximum_iops),
        ] {
            let path = ["ebsInfo", "ebsOptimizedInfo", name];
            let reported = figure(case, &body, &path) as i64;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {name} {expected}, got {reported}"
            );
        }
        // The throughput members are the bandwidth ones in the other unit, which is how the
        // published table's Mbps and MB/s columns relate.
        for (name, bandwidth) in [
            ("baselineThroughputInMBps", baseline_mbps),
            ("maximumThroughputInMBps", maximum_mbps),
        ] {
            let expected = bandwidth as f64 / MEGABITS_PER_MEGABYTE;
            let reported = figure(case, &body, &["ebsInfo", "ebsOptimizedInfo", name]);
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected {name} {expected}, got {reported}"
            );
        }

        let (attachments, limit_type) = row.ebs_attachments;
        if let Some(attachments) = attachments {
            let reported = figure(case, &body, &["ebsInfo", "maximumEbsAttachments"]) as i64;
            assert_eq!(
                reported, attachments,
                "case {case:?}: expected maximumEbsAttachments {attachments}, got {reported}"
            );
        }
        let reported = member(case, &body, &["ebsInfo", "attachmentLimitType"]);
        assert_eq!(
            reported, limit_type,
            "case {case:?}: expected attachmentLimitType {limit_type:?}, got {reported:?}"
        );

        // The cards are reported only for a type that carries EBS hardware of its own, each
        // one taking an equal share of the type's figures.
        match row.ebs_cards {
            Some(cards) => {
                let reported = figure(case, &body, &["ebsInfo", "maximumEbsCards"]) as usize;
                assert_eq!(
                    reported, cards,
                    "case {case:?}: expected maximumEbsCards {cards}, got {reported}"
                );
                let reported =
                    reported_instance_type_list(case, &body, &["ebsInfo", "ebsCardSet"]).len();
                assert_eq!(
                    reported, cards,
                    "case {case:?}: expected {cards} reported EBS cards, got {reported}"
                );
            }
            None => assert!(
                reported_instance_type_member(case, &body, &["ebsInfo", "maximumEbsCards"])
                    .is_none(),
                "case {case:?}: a type whose volumes come out of the shared pool reports no \
                 EBS cards of its own, got {body}"
            ),
        }

        match row.instance_store {
            Some((devices, gb_each)) => {
                let reported =
                    figure(case, &body, &["instanceStorageInfo", "disks", "item", "count"]) as i64;
                assert_eq!(
                    reported, devices,
                    "case {case:?}: expected {devices} local devices, got {reported}"
                );
                let reported = figure(
                    case,
                    &body,
                    &["instanceStorageInfo", "disks", "item", "sizeInGB"],
                ) as i64;
                assert_eq!(
                    reported, gb_each,
                    "case {case:?}: expected local devices of {gb_each} GB, got {reported}"
                );
                let reported =
                    figure(case, &body, &["instanceStorageInfo", "totalSizeInGB"]) as i64;
                assert_eq!(
                    reported,
                    devices * gb_each,
                    "case {case:?}: expected totalSizeInGB {}, got {reported}",
                    devices * gb_each
                );
            }
            None => assert!(
                reported_instance_type_member(case, &body, &["instanceStorageInfo"]).is_none(),
                "case {case:?}: a type with no local devices reports no instanceStorageInfo, \
                 got {body}"
            ),
        }
    }
}

/// The network, processor and host-feature row of one instance type: the capabilities a
/// client gates a feature on.
#[derive(Debug, Clone, Copy)]
struct CapabilityRow {
    /// `c8in.12xlarge` and the like.
    instance_type: &'static str,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// The same published figure in Gbps, as (the bandwidth it sustains, the bandwidth it can
    /// reach) — what its single network card reports, that card carrying the whole instance's
    /// network.
    network_bandwidth: (f64, f64),
    /// The Elastic Fabric Adapters it is offered, and `None` where it is offered none at all.
    efa: Option<i64>,
    /// Whether ENA Express is offered on it.
    ena_srd: bool,
    /// The idle timeout, in seconds, its connection tracking applies to an established TCP
    /// connection by default.
    tcp_established_timeout: i64,
    /// Its processor's sustained all-core frequency, in GHz.
    sustained_ghz: f64,
    /// Whether reboot migration is offered, and whether a local PTP hardware clock is.
    host_features: (bool, bool),
    /// Whether the packets of a session can be copied under its guest.
    traffic_mirroring: bool,
    /// Whether CloudWatch action based recovery is offered.
    auto_recovery: bool,
    /// The boot modes it is offered, where AWS's published list for it is the type's own.
    boot_modes: Option<&'static [&'static str]>,
}

/// The boot modes a bare metal size of an x86 family is offered: the legacy BIOS mode alone.
const LEGACY_BIOS_ONLY: &[&str] = &["legacy-bios"];

/// A representative size of each shape the rule covers in both families: the smallest size,
/// one that carries the family's whole dedicated share, the widest virtualized one and each
/// bare metal one.
const CAPABILITY_ROWS: [CapabilityRow; 9] = [
    CapabilityRow {
        instance_type: "c8in.large",
        network_performance: "Up to 25 Gigabit",
        network_bandwidth: (3.125, 25.0),
        efa: None,
        ena_srd: false,
        tcp_established_timeout: 350,
        sustained_ghz: 3.9,
        host_features: (true, false),
        traffic_mirroring: false,
        auto_recovery: true,
        boot_modes: None,
    },
    CapabilityRow {
        instance_type: "c8in.12xlarge",
        network_performance: "75 Gigabit",
        network_bandwidth: (75.0, 75.0),
        efa: None,
        ena_srd: true,
        tcp_established_timeout: 350,
        sustained_ghz: 3.9,
        host_features: (true, false),
        traffic_mirroring: false,
        auto_recovery: true,
        boot_modes: None,
    },
    CapabilityRow {
        instance_type: "c8in.48xlarge",
        network_performance: "300 Gigabit",
        network_bandwidth: (300.0, 300.0),
        efa: Some(1),
        ena_srd: true,
        tcp_established_timeout: 350,
        sustained_ghz: 3.9,
        host_features: (true, false),
        traffic_mirroring: false,
        auto_recovery: true,
        boot_modes: None,
    },
    CapabilityRow {
        instance_type: "c8in.96xlarge",
        network_performance: "600 Gigabit",
        network_bandwidth: (600.0, 600.0),
        efa: Some(2),
        ena_srd: true,
        tcp_established_timeout: 350,
        sustained_ghz: 3.9,
        host_features: (true, false),
        traffic_mirroring: false,
        auto_recovery: true,
        boot_modes: None,
    },
    CapabilityRow {
        instance_type: "c8in.metal-96xl",
        network_performance: "600 Gigabit",
        network_bandwidth: (600.0, 600.0),
        efa: Some(2),
        ena_srd: true,
        tcp_established_timeout: 350,
        sustained_ghz: 3.9,
        host_features: (true, false),
        traffic_mirroring: false,
        auto_recovery: true,
        boot_modes: Some(LEGACY_BIOS_ONLY),
    },
    CapabilityRow {
        instance_type: "x2idn.16xlarge",
        network_performance: "50 Gigabit",
        network_bandwidth: (50.0, 50.0),
        efa: None,
        ena_srd: true,
        tcp_established_timeout: 432_000,
        sustained_ghz: 3.5,
        host_features: (false, false),
        traffic_mirroring: true,
        auto_recovery: true,
        boot_modes: None,
    },
    CapabilityRow {
        instance_type: "x2idn.24xlarge",
        network_performance: "75 Gigabit",
        network_bandwidth: (75.0, 75.0),
        efa: None,
        ena_srd: true,
        tcp_established_timeout: 432_000,
        sustained_ghz: 3.5,
        host_features: (false, false),
        traffic_mirroring: true,
        auto_recovery: true,
        boot_modes: None,
    },
    CapabilityRow {
        instance_type: "x2idn.32xlarge",
        network_performance: "100 Gigabit",
        network_bandwidth: (100.0, 100.0),
        efa: Some(1),
        ena_srd: true,
        tcp_established_timeout: 432_000,
        sustained_ghz: 3.5,
        host_features: (false, false),
        traffic_mirroring: true,
        auto_recovery: true,
        boot_modes: None,
    },
    CapabilityRow {
        instance_type: "x2idn.metal",
        network_performance: "100 Gigabit",
        network_bandwidth: (100.0, 100.0),
        efa: Some(1),
        ena_srd: true,
        tcp_established_timeout: 432_000,
        sustained_ghz: 3.5,
        host_features: (false, false),
        traffic_mirroring: false,
        auto_recovery: true,
        boot_modes: Some(LEGACY_BIOS_ONLY),
    },
];

/// Rule: the network, processor and host-feature members a type reports are the published
/// facts of THAT type's hardware, so two families of one size answer differently and one
/// family's bare metal size answers differently from its virtualized ones — and the type's
/// single network card reports the bandwidth its own published network performance states.
#[tokio::test]
async fn published_capability_rows_are_the_facts_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in CAPABILITY_ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let reported = member(case, &body, &["networkInfo", "networkPerformance"]);
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );

        // Every one of these types carries ONE network card, which therefore carries the
        // whole instance's network: the card reports the published figure of the line above,
        // in Gbps, and the same text for its own performance.
        let reported = figure(case, &body, &["networkInfo", "maximumNetworkCards"]) as i64;
        assert_eq!(
            reported, 1,
            "case {case:?}: expected one network card, got {reported}"
        );
        let reported = member(
            case,
            &body,
            &["networkInfo", "networkCards", "item", "networkPerformance"],
        );
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected the card's networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );
        let (baseline_gbps, peak_gbps) = row.network_bandwidth;
        for (name, expected) in [
            ("baselineBandwidthInGbps", baseline_gbps),
            ("peakBandwidthInGbps", peak_gbps),
        ] {
            let reported = figure(
                case,
                &body,
                &["networkInfo", "networkCards", "item", name],
            );
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected the card's {name} {expected}, got {reported}"
            );
        }

        let reported = member(case, &body, &["networkInfo", "efaSupported"]);
        let expected = if row.efa.is_some() { "true" } else { "false" };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected efaSupported {expected}, got {reported}"
        );
        match row.efa {
            Some(adapters) => {
                let reported = figure(
                    case,
                    &body,
                    &["networkInfo", "efaInfo", "maximumEfaInterfaces"],
                ) as i64;
                assert_eq!(
                    reported, adapters,
                    "case {case:?}: expected maximumEfaInterfaces {adapters}, got {reported}"
                );
            }
            None => assert!(
                reported_instance_type_member(case, &body, &["networkInfo", "efaInfo"]).is_none(),
                "case {case:?}: a type offered no Elastic Fabric Adapter reports no efaInfo, \
                 got {body}"
            ),
        }

        let reported = member(case, &body, &["networkInfo", "enaSrdSupported"]);
        assert_eq!(
            reported,
            row.ena_srd.to_string(),
            "case {case:?}: expected enaSrdSupported {}, got {reported}",
            row.ena_srd
        );

        let reported = figure(
            case,
            &body,
            &[
                "networkInfo",
                "connectionTrackingConfiguration",
                "defaultTcpEstablishedTimeout",
            ],
        ) as i64;
        assert_eq!(
            reported, row.tcp_established_timeout,
            "case {case:?}: expected defaultTcpEstablishedTimeout {}, got {reported}",
            row.tcp_established_timeout
        );

        let reported = figure(case, &body, &["processorInfo", "sustainedClockSpeedInGhz"]);
        assert!(
            (reported - row.sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {}, got {reported}",
            row.sustained_ghz
        );

        let (reboot_migration, phc) = row.host_features;
        for (path, supported) in [
            (vec!["rebootMigrationSupport"], reboot_migration),
            (vec!["phcSupport"], phc),
            (
                vec!["networkInfo", "trafficMirroringSupport"],
                row.traffic_mirroring,
            ),
        ] {
            let expected = if supported { "supported" } else { "unsupported" };
            let reported = member(case, &body, &path);
            assert_eq!(
                reported,
                expected,
                "case {case:?}: expected {} {expected}, got {reported}",
                path.join("/")
            );
        }

        let reported = member(case, &body, &["autoRecoverySupported"]);
        assert_eq!(
            reported,
            row.auto_recovery.to_string(),
            "case {case:?}: expected autoRecoverySupported {}, got {reported}",
            row.auto_recovery
        );

        if let Some(modes) = row.boot_modes {
            let reported = reported_instance_type_list(case, &body, &["supportedBootModes"]);
            assert_eq!(
                reported,
                owned(modes),
                "case {case:?}: expected supportedBootModes {modes:?}, got {reported:?}"
            );
        }

        // Neither family takes an interface attachment from a secondary network, and so
        // reports no secondary interface limit either.
        let reported = member(case, &body, &["networkInfo", "secondaryNetworkSupported"]);
        assert_eq!(
            reported, "false",
            "case {case:?}: expected secondaryNetworkSupported false, got {reported}"
        );
        assert!(
            reported_instance_type_member(
                case,
                &body,
                &["networkInfo", "maximumSecondaryNetworkInterfaces"]
            )
            .is_none(),
            "case {case:?}: a type that takes no secondary attachment reports no secondary \
             interface limit, got {body}"
        );
    }
}

/// The core counts one instance type publishes (`VCpuInfo$ValidCores`), as (the type, the
/// smallest count, the step the counts go up in, the type's own default): a type whose cores
/// sit on more than one processor socket takes them one per socket, so its counts go up in
/// that step from the smallest count its hardware is offered with, a type that takes them one
/// at a time publishes every count up to its default, and a type that takes them ALL at once
/// publishes that default alone.
const VALID_CORE_ROWS: [(&str, i32, i32, i32); 5] = [
    ("c8in.12xlarge", 1, 1, 24),
    ("c8in.24xlarge", 4, 2, 48),
    ("c8in.48xlarge", 96, 96, 96),
    ("x2idn.24xlarge", 2, 2, 48),
    ("x2idn.metal", 1, 1, 64),
];

/// Rule: the core counts a type publishes are the published list of THAT type, so two sizes
/// of one family publish different lists and one size of two families does too.
#[tokio::test]
async fn published_core_counts_are_the_list_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for (instance_type, smallest, step, default_cores) in VALID_CORE_ROWS {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported: Vec<i32> = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"])
            .iter()
            .map(|count| {
                count.parse().unwrap_or_else(|_| {
                    panic!("case {case:?}: a core count must be a number, got {count:?}")
                })
            })
            .collect();
        let expected: Vec<i32> = (smallest..=default_cores)
            .filter(|cores| cores % step == 0)
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected validCores {expected:?}, got {reported:?}"
        );
    }
}

/// The `double`-typed members of a reported instance type, with the text each is rendered as:
/// a whole published figure carries its fraction, and one that is not whole carries the
/// digits it needs. Every entry names the type, the path to the member and that text.
const RENDERED_DOUBLES: [(&str, &[&str], &str); 6] = [
    (
        "c8in.12xlarge",
        &["ebsInfo", "ebsOptimizedInfo", "baselineThroughputInMBps"],
        "1875.0",
    ),
    (
        "c8in.12xlarge",
        &["ebsInfo", "ebsOptimizedInfo", "maximumThroughputInMBps"],
        "1875.0",
    ),
    (
        "x2idn.24xlarge",
        &["ebsInfo", "ebsOptimizedInfo", "baselineThroughputInMBps"],
        "7500.0",
    ),
    (
        "c8in.12xlarge",
        &["processorInfo", "sustainedClockSpeedInGhz"],
        "3.9",
    ),
    (
        "c8in.12xlarge",
        &["networkInfo", "networkCards", "item", "baselineBandwidthInGbps"],
        "75.0",
    ),
    (
        "c8in.large",
        &["networkInfo", "networkCards", "item", "baselineBandwidthInGbps"],
        "3.125",
    ),
];

/// Rule: a member the model types as a `double` is rendered as a double — a whole figure
/// carries its fraction, so a client's parser reads a floating point number and not an
/// integer. The rendering is the envelope's
/// (`service_common::query_decimal_floats`, which wraps every success body of this service);
/// what this pins is that the published figures above reach the wire through it, the ones
/// that carry a fraction of their unit included.
#[tokio::test]
async fn a_double_member_is_rendered_with_its_fraction() {
    let (router, _engine) = super::ec2_query::emulator();

    for (instance_type, path, expected) in RENDERED_DOUBLES {
        let case = format!("{instance_type} {}", path.join("/"));
        let body = described(&case, &router, instance_type).await;
        let reported = member(&case, &body, path);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected <{}> rendered as {expected:?}, got {reported:?}",
            path.join("/")
        );
    }
}
