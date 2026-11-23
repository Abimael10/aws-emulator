// SPDX-License-Identifier: CC0-1.0

//! The published rows of the two Intel storage optimized instance-type families, driven over
//! the wire this service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST
//! answered with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one `published_instance_type_hardware_facts` pins: AN INSTANCE
//! TYPE'S HARDWARE AND CAPABILITIES ARE PUBLISHED FACTS OF THAT TYPE, read off a per-type
//! table, not computed from the vCPUs and memory its name gives. These two families are where
//! that bites hardest: the EBS figures of one of them do not scale by the size multiplier at
//! all, its volume limit is shared with its devices where a Nitro family's is the volumes' own,
//! the other family's local devices are a different size from the one its name reads as, and
//! both publish host features — hibernation on a storage family, ENA Express from one rung of
//! the ladder up, a bare metal size offered one boot mode alone — that no function of the name
//! yields.
//!
//! Every table below spans what the rule generalises over: two families a hardware generation
//! apart (one pre-Nitro, one Nitro), the smallest, a middle, the widest virtualized and a bare
//! metal size of each, sizes either side of every capability threshold, and — for each
//! capability member — one type that has it beside one that does not. The sizes the trace that
//! filed this happened to name are rows among many, and the closing test holds the two families
//! against each other at ONE size, so no row of these tables can be read as the specification.

use super::ec2_query::{
    assert_published_ebs, described_instance_type, element, elements, number,
    reported_instance_type_list, required,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The published hardware figures of one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `i7i.2xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (f64, f64, f64, f64),
    /// Whether its EBS-optimized mode is on whatever the launch asks (`default`) or is an
    /// option of the launch (`supported`).
    ebs_optimized: &'static str,
    /// The EBS volumes it can hold, and whether that limit is shared with its other devices
    /// or is the EBS devices' own — `None` for a type whose published limit the catalog does
    /// not hold, which the derivation answers for.
    ebs_attachments: Option<(i64, &'static str)>,
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its processor's maker and that processor's sustained frequency, in GHz.
    processor: (&'static str, f64),
}

/// Rule: the EBS, instance store, network and processor figures of both families are the ones
/// AWS publishes for each type.
#[tokio::test]
async fn hardware_rows_of_the_intel_storage_families_are_the_published_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: four sizes and the bare metal one of the pre-Nitro family, whose EBS-optimized mode
    // is on at every size and carries ONE figure rather than a baseline it bursts above, and
    // whose figures follow no uniform slope; and six sizes of the Nitro family, which bursts
    // above its baseline below the size that carries a dedicated share, holds its EBS volumes on
    // hardware of its own up to its widest virtualized size and shares its host's one pool of
    // attachments on each bare metal size.
    let rows = [
        Published {
            instance_type: "i3.large",
            ebs: (425.0, 3_000.0, 425.0, 3_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((40, "shared")),
            instance_store: Some((1, 475)),
            network_performance: "Up to 10 Gigabit",
            processor: ("Intel", 2.3),
        },
        Published {
            instance_type: "i3.2xlarge",
            ebs: (1_700.0, 12_000.0, 1_700.0, 12_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((40, "shared")),
            instance_store: Some((1, 1_900)),
            network_performance: "Up to 10 Gigabit",
            processor: ("Intel", 2.3),
        },
        Published {
            instance_type: "i3.8xlarge",
            ebs: (7_000.0, 32_500.0, 7_000.0, 32_500.0),
            ebs_optimized: "default",
            ebs_attachments: Some((40, "shared")),
            instance_store: Some((4, 1_900)),
            network_performance: "10 Gigabit",
            processor: ("Intel", 2.3),
        },
        Published {
            instance_type: "i3.16xlarge",
            ebs: (14_000.0, 65_000.0, 14_000.0, 65_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((40, "shared")),
            instance_store: Some((8, 1_900)),
            network_performance: "25 Gigabit",
            processor: ("Intel", 2.3),
        },
        Published {
            instance_type: "i3.metal",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            ebs_optimized: "default",
            // The volume limit of the whole host is not a figure the catalog holds for this
            // type, so the row publishes none and the derivation answers it.
            ebs_attachments: None,
            instance_store: Some((8, 1_900)),
            network_performance: "25 Gigabit",
            processor: ("Intel", 2.3),
        },
        Published {
            instance_type: "i7i.large",
            ebs: (625.0, 2_500.0, 10_000.0, 40_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((32, "dedicated")),
            instance_store: Some((1, 468)),
            network_performance: "Up to 10 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i7i.2xlarge",
            ebs: (2_500.0, 10_000.0, 10_000.0, 40_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((32, "dedicated")),
            instance_store: Some((1, 1_875)),
            network_performance: "Up to 12 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i7i.8xlarge",
            ebs: (10_000.0, 40_000.0, 10_000.0, 40_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((32, "dedicated")),
            instance_store: Some((2, 3_750)),
            network_performance: "Up to 25 Gigabit",
            processor: ("Intel", 3.2),
        },
        // The three sizes whose network performance is the share of the host the family's
        // published slope gives them — 0.586 Gigabit per vCPU — rather than the ceiling the
        // sizes below them burst to.
        Published {
            instance_type: "i7i.12xlarge",
            ebs: (15_000.0, 60_000.0, 15_000.0, 60_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((32, "dedicated")),
            instance_store: Some((3, 3_750)),
            network_performance: "Up to 28.12 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i7i.16xlarge",
            ebs: (20_000.0, 80_000.0, 20_000.0, 80_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((48, "dedicated")),
            instance_store: Some((4, 3_750)),
            network_performance: "Up to 37.5 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i7i.24xlarge",
            ebs: (30_000.0, 120_000.0, 30_000.0, 120_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((64, "dedicated")),
            instance_store: Some((6, 3_750)),
            network_performance: "Up to 56.25 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i7i.48xlarge",
            ebs: (60_000.0, 240_000.0, 60_000.0, 240_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((128, "dedicated")),
            instance_store: Some((12, 3_750)),
            network_performance: "Up to 100 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i7i.metal-24xl",
            ebs: (30_000.0, 120_000.0, 30_000.0, 120_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((39, "shared")),
            instance_store: Some((6, 3_750)),
            network_performance: "Up to 56.25 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i7i.metal-48xl",
            ebs: (60_000.0, 240_000.0, 60_000.0, 240_000.0),
            ebs_optimized: "default",
            ebs_attachments: Some((79, "shared")),
            instance_store: Some((12, 3_750)),
            network_performance: "Up to 100 Gigabit",
            processor: ("Intel", 3.2),
        },
    ];

    for row in rows {
        let case = row.instance_type;
        let body = described_instance_type(case, &router, REGION, row.instance_type).await;
        // The six `EbsOptimizedInfo` readings, against the four figures AWS publishes.
        assert_published_ebs(case, &body, row.ebs);

        let ebs_info = required(case, &body, "ebsInfo");
        let reported = required(case, &ebs_info, "ebsOptimizedSupport");
        assert_eq!(
            reported, row.ebs_optimized,
            "case {case:?}: expected ebsOptimizedSupport {:?}, got {reported:?}",
            row.ebs_optimized
        );
        match row.ebs_attachments {
            Some((attachments, limit_type)) => {
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
            }
            // A row that publishes no limit still reports one, and it is a figure of the type's
            // own hardware rather than nothing: the derivation answers it.
            None => {
                let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
                assert!(
                    reported > 0,
                    "case {case:?}: expected a derived maximumEbsAttachments, got {reported}"
                );
            }
        }

        let storage = element(&body, "instanceStorageInfo");
        match row.instance_store {
            Some((devices, gb_each)) => {
                let storage = storage.unwrap_or_else(|| {
                    panic!("case {case:?}: expected an instanceStorageInfo, got {body}")
                });
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
                storage.is_none(),
                "case {case:?}: a type with no local devices reports no instanceStorageInfo, \
                 got {storage:?}"
            ),
        }

        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "networkPerformance");
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );

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
    }
}

/// Rule: the capabilities and host features a type is offered with are published per type, so
/// neither the family letter nor the hardware generation answers for every type on it.
#[tokio::test]
async fn capabilities_of_the_intel_storage_families_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: for each member, one type that has the capability beside one that does not — the
    // rung of the ladder from which ENA Express is offered, the configurable queue count and the
    // secondary-network attachments the generation would otherwise give, and the Elastic Fabric
    // Adapter on the widest sizes beside the smallest.
    let network_rows = [
        ("i3.large", "enaSrdSupported", "false"),
        ("i3.16xlarge", "enaSrdSupported", "false"),
        ("i7i.large", "enaSrdSupported", "false"),
        ("i7i.8xlarge", "enaSrdSupported", "false"),
        ("i7i.12xlarge", "enaSrdSupported", "true"),
        ("i7i.48xlarge", "enaSrdSupported", "true"),
        ("i7i.metal-24xl", "enaSrdSupported", "true"),
        ("i7i.4xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("i7i.metal-48xl", "flexibleEnaQueuesSupport", "unsupported"),
        ("i7i.16xlarge", "secondaryNetworkSupported", "false"),
        ("i7i.16xlarge", "ipv4AddressesPerSecondaryInterface", "0"),
        ("i7i.24xlarge", "efaSupported", "true"),
        ("i7i.metal-48xl", "efaSupported", "true"),
        ("i7i.large", "efaSupported", "false"),
        ("i3.16xlarge", "efaSupported", "false"),
    ];

    for (instance_type, member, expected) in network_rows {
        let case = format!("{instance_type} {member}");
        let body = described_instance_type(&case, &router, REGION, instance_type).await;
        let network_info = required(&case, &body, "networkInfo");
        let reported = required(&case, &network_info, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The host features, each a flag a client branches on: hibernation either side of the memory
    // the prerequisites bound an instance at, a PTP hardware clock and reboot migration on a
    // family whose local devices a migration could not carry over, a trusted platform module on
    // a virtualized size beside a bare metal one, and the burst model of a type's vCPUs.
    let feature_rows = [
        ("i3.large", "hibernationSupported", "true"),
        ("i3.4xlarge", "hibernationSupported", "true"),
        ("i3.8xlarge", "hibernationSupported", "false"),
        ("i3.16xlarge", "hibernationSupported", "false"),
        ("i7i.xlarge", "hibernationSupported", "true"),
        ("i7i.4xlarge", "hibernationSupported", "true"),
        ("i7i.8xlarge", "hibernationSupported", "false"),
        ("i7i.metal-24xl", "hibernationSupported", "false"),
        ("i3.large", "phcSupport", "unsupported"),
        ("i7i.large", "phcSupport", "supported"),
        ("i7i.metal-48xl", "phcSupport", "supported"),
        ("i3.xlarge", "rebootMigrationSupport", "unsupported"),
        ("i7i.large", "rebootMigrationSupport", "unsupported"),
        ("i7i.48xlarge", "rebootMigrationSupport", "unsupported"),
        ("i7i.metal-24xl", "rebootMigrationSupport", "unsupported"),
        ("i7i.large", "nitroTpmSupport", "supported"),
        ("i7i.metal-24xl", "nitroTpmSupport", "unsupported"),
        ("i7i.metal-48xl", "nitroTpmSupport", "unsupported"),
        ("i3.large", "cpuBurstModel", "none"),
        ("i7i.24xlarge", "cpuBurstModel", "none"),
    ];

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described_instance_type(&case, &router, REGION, instance_type).await;
        let reported = required(&case, &body, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The boot modes a bare metal size of the Nitro family is offered — one alone, where a
    // virtualized size of the same family is offered both — and the processor features its
    // virtualized sizes publish, which a bare metal size running no hypervisor does not.
    for (instance_type, expected) in [
        ("i7i.metal-24xl", vec!["legacy-bios"]),
        ("i7i.metal-48xl", vec!["legacy-bios"]),
        ("i7i.large", vec!["legacy-bios", "uefi"]),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let reported = reported_instance_type_list(case, &body, &["supportedBootModes"]);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {reported:?}"
        );
    }
    for (instance_type, expected) in [
        ("i7i.large", vec!["nested-virtualization"]),
        ("i7i.24xlarge", vec!["nested-virtualization"]),
        ("i7i.metal-48xl", Vec::new()),
        ("i3.2xlarge", Vec::new()),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let processor_info = required(case, &body, "processorInfo");
        let reported = match element(&processor_info, "supportedFeatures") {
            Some(features) => elements(&features, "item"),
            None => Vec::new(),
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected processor supportedFeatures {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, the sharp edge: the published rows are NOT a function of the vCPUs and memory the name
/// gives. Two families at ONE size disagree on every figure, one family's maximum IS its
/// baseline at every size while the other's bursts above it, and the core counts a caller may
/// configure are the published list rather than every count up to the default.
#[tokio::test]
async fn the_intel_storage_rows_are_not_a_function_of_the_size() {
    let (router, _engine) = super::ec2_query::emulator();

    let ebs_pair = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumBandwidthInMbps") as i64,
        )
    };

    // Two families, ONE size — 32 vCPUs each — disagreeing on the EBS figures, the network
    // performance, the size of their local devices and even the KIND of their attachment limit.
    let left = described_instance_type("i3.8xlarge", &router, REGION, "i3.8xlarge").await;
    let right = described_instance_type("i7i.8xlarge", &router, REGION, "i7i.8xlarge").await;
    for (case, body) in [("i3.8xlarge", &left), ("i7i.8xlarge", &right)] {
        let vcpus = number(case, body, "defaultVCpus") as i64;
        assert_eq!(
            vcpus, 32,
            "case {case:?}: both rows must be the same size, got {vcpus} vCPUs"
        );
    }
    assert_ne!(
        ebs_pair(&left, "i3.8xlarge"),
        ebs_pair(&right, "i7i.8xlarge"),
        "two families of one size publish different EBS figures"
    );
    for member in ["attachmentLimitType", "maximumEbsAttachments"] {
        assert_ne!(
            required("i3.8xlarge", &required("i3.8xlarge", &left, "ebsInfo"), member),
            required(
                "i7i.8xlarge",
                &required("i7i.8xlarge", &right, "ebsInfo"),
                member
            ),
            "two families of one size publish a different {member}"
        );
    }
    assert_ne!(
        required(
            "i3.8xlarge",
            &required("i3.8xlarge", &left, "instanceStorageInfo"),
            "totalSizeInGB"
        ),
        required(
            "i7i.8xlarge",
            &required("i7i.8xlarge", &right, "instanceStorageInfo"),
            "totalSizeInGB"
        ),
        "two families of one size publish a different instance store"
    );

    // One family, four sizes: the pre-Nitro one sustains its ceiling at EVERY size, which no
    // "a small size bursts above its baseline" rule produces.
    for instance_type in ["i3.large", "i3.2xlarge", "i3.8xlarge", "i3.metal"] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let (baseline, maximum) = ebs_pair(&body, case);
        assert_eq!(
            baseline, maximum,
            "case {case:?}: this family has one dedicated figure, got {baseline}/{maximum}"
        );
    }
    // While the Nitro family's small sizes burst above their baseline and its widest ones
    // sustain their ceiling, which is the other half of the same rule.
    for (instance_type, bursts) in [
        ("i7i.large", true),
        ("i7i.4xlarge", true),
        ("i7i.16xlarge", false),
        ("i7i.metal-48xl", false),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let (baseline, maximum) = ebs_pair(&body, case);
        assert_eq!(
            maximum > baseline,
            bursts,
            "case {case:?}: expected bursts={bursts}, got {baseline}/{maximum}"
        );
    }

    // The core counts a caller may configure: a size whose cores sit on two processor sockets
    // publishes them one per socket, the size that takes its cores all at once publishes the one
    // count it has, and the size below both publishes every count up to its default.
    for (instance_type, expected) in [
        ("i3.16xlarge", (1..=16).map(|core| core * 2).collect()),
        ("i7i.48xlarge", vec![96_i64]),
        ("i3.8xlarge", (1..=16).collect::<Vec<i64>>()),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
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
