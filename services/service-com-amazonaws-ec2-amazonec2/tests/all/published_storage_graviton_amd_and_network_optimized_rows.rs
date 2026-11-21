// SPDX-License-Identifier: CC0-1.0

//! The published rows of three more instance-type families — a storage optimized AWS Graviton
//! family with local Nitro SSD devices, a first generation AMD EPYC general purpose family and
//! an Ice Lake general purpose family built for the network — driven over the wire this service
//! speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML;
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one `published_instance_type_hardware_facts` pins: AN INSTANCE
//! TYPE'S HARDWARE AND CAPABILITIES ARE PUBLISHED FACTS OF THAT TYPE, read off a per-type
//! table, not computed from the vCPUs its name gives. So one family's sustained EBS bandwidth
//! is a figure its size multiplier never yields, another's local devices change size up the
//! ladder while their count only grows at the top, a third reports on each of the two cards its
//! widest sizes carry the figure that card carries, and the host features each is offered —
//! hibernation, an enclave, a trusted platform module, a Dedicated Host, reboot migration and a
//! local PTP hardware clock — are per-type answers that no hardware generation gives for every
//! family on it.
//!
//! Every table below spans what the rule generalises over: three families of three different
//! designs, the smallest, a middle, the largest virtualized and (where the family has one) a
//! bare metal size of each, a family with local devices and two without, a family whose
//! baseline rises to its ceiling and one whose sizes burst well above theirs, and — for each
//! capability member — one type that has it beside one that does not. The sizes the trace that
//! filed this happened to name are rows among many, and the closing test holds two families at
//! ONE size against each other, so no row of these tables can be read as the specification.

use super::ec2_query::{
    described_instance_type, element, number, reported_instance_type_list,
    reported_instance_type_member, required, MEGABITS_PER_MEGABYTE,
};
use super::wire::{child_text, document, set_items};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The halves of an Mbps in one Mbps: a published sustained bandwidth that is not a whole
/// number of Mbps (1,562.5) is a whole number of these, which is the unit a row below states
/// it in so that the integer `baselineBandwidthInMbps` member and the fractional
/// `baselineThroughputInMBps` one are read off ONE figure.
const HALVES_PER_MBPS: f64 = 2.0;

/// The published hardware figures of one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `is4gen.4xlarge` and the like.
    instance_type: &'static str,
    /// The bandwidth it sustains, in HALVES of an Mbps.
    baseline_half_mbps: i64,
    /// The I/O operations per second it sustains.
    baseline_iops: i64,
    /// The bandwidth, in Mbps, it can reach.
    maximum_mbps: i64,
    /// The I/O operations per second it can reach.
    maximum_iops: i64,
    /// The EBS volumes it can hold attached, and whether that limit is shared with its other
    /// devices or is the EBS devices' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its processor's maker and that processor's sustained frequency, in GHz.
    processor: (&'static str, f64),
}

/// The rows the two tests below read, one per instance type.
///
/// Four sizes of the storage optimized Graviton family — whose devices change size up the
/// ladder while their count only grows at the top, and whose shared attachment pool therefore
/// leaves fewer EBS volumes as it does; four sizes of the AMD family, whose sustained figure
/// rises to its own dedicated ceiling from the middle of the ladder up; and four sizes of the
/// network optimized Intel family, whose smallest size sustains half an Mbps and whose two
/// widest sizes carry the whole host.
const PUBLISHED_ROWS: &[Published] = &[
    Published {
        instance_type: "is4gen.medium",
        baseline_half_mbps: 1_250,
        baseline_iops: 2_500,
        maximum_mbps: 10_000,
        maximum_iops: 40_000,
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 937)),
        network_performance: "Up to 25 Gigabit",
        processor: ("AWS", 2.5),
    },
    Published {
        instance_type: "is4gen.large",
        baseline_half_mbps: 2_500,
        baseline_iops: 5_000,
        maximum_mbps: 10_000,
        maximum_iops: 40_000,
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 1_875)),
        network_performance: "Up to 25 Gigabit",
        processor: ("AWS", 2.5),
    },
    Published {
        instance_type: "is4gen.4xlarge",
        baseline_half_mbps: 20_000,
        baseline_iops: 40_000,
        maximum_mbps: 10_000,
        maximum_iops: 40_000,
        ebs_attachments: (25, "shared"),
        instance_store: Some((2, 7_500)),
        network_performance: "25 Gigabit",
        processor: ("AWS", 2.5),
    },
    Published {
        instance_type: "is4gen.8xlarge",
        baseline_half_mbps: 40_000,
        baseline_iops: 80_000,
        maximum_mbps: 20_000,
        maximum_iops: 80_000,
        ebs_attachments: (23, "shared"),
        instance_store: Some((4, 7_500)),
        network_performance: "50 Gigabit",
        processor: ("AWS", 2.5),
    },
    Published {
        instance_type: "m5a.large",
        baseline_half_mbps: 1_300,
        baseline_iops: 3_600,
        maximum_mbps: 2_880,
        maximum_iops: 16_000,
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "Up to 10 Gigabit",
        processor: ("AMD", 2.5),
    },
    Published {
        instance_type: "m5a.xlarge",
        baseline_half_mbps: 2_170,
        baseline_iops: 6_000,
        maximum_mbps: 2_880,
        maximum_iops: 16_000,
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "Up to 10 Gigabit",
        processor: ("AMD", 2.5),
    },
    Published {
        instance_type: "m5a.12xlarge",
        baseline_half_mbps: 13_560,
        baseline_iops: 30_000,
        maximum_mbps: 6_780,
        maximum_iops: 30_000,
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "10 Gigabit",
        processor: ("AMD", 2.5),
    },
    Published {
        instance_type: "m5a.24xlarge",
        baseline_half_mbps: 27_500,
        baseline_iops: 60_000,
        maximum_mbps: 13_750,
        maximum_iops: 60_000,
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "20 Gigabit",
        processor: ("AMD", 2.5),
    },
    Published {
        instance_type: "m6in.large",
        baseline_half_mbps: 3_125,
        baseline_iops: 6_250,
        maximum_mbps: 25_000,
        maximum_iops: 100_000,
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "Up to 25 Gigabit",
        processor: ("Intel", 3.5),
    },
    Published {
        instance_type: "m6in.8xlarge",
        baseline_half_mbps: 50_000,
        baseline_iops: 100_000,
        maximum_mbps: 25_000,
        maximum_iops: 100_000,
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "50 Gigabit",
        processor: ("Intel", 3.5),
    },
    Published {
        instance_type: "m6in.32xlarge",
        baseline_half_mbps: 200_000,
        baseline_iops: 400_000,
        maximum_mbps: 100_000,
        maximum_iops: 400_000,
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "200 Gigabit",
        processor: ("Intel", 3.5),
    },
    Published {
        instance_type: "m6in.metal",
        baseline_half_mbps: 200_000,
        baseline_iops: 400_000,
        maximum_mbps: 100_000,
        maximum_iops: 400_000,
        ebs_attachments: (31, "shared"),
        instance_store: None,
        network_performance: "200 Gigabit",
        processor: ("Intel", 3.5),
    },
];

/// Rule: the EBS, instance store, network and processor figures of these three families are the
/// ones AWS publishes for each type.
#[tokio::test]
async fn hardware_rows_of_three_more_families_are_the_published_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in PUBLISHED_ROWS {
        let case = row.instance_type;
        let body = described_instance_type(case, &router, REGION, row.instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");

        // The integer bandwidth member cannot hold a half of an Mbps, so it reports the whole
        // Mbps of the published figure while the throughput member carries the megabytes of the
        // WHOLE figure, half included — which is how the published table's two columns relate.
        let baseline_mbps = row.baseline_half_mbps / 2;
        for (member, expected) in [
            ("baselineBandwidthInMbps", baseline_mbps as f64),
            ("baselineIops", row.baseline_iops as f64),
            ("maximumBandwidthInMbps", row.maximum_mbps as f64),
            ("maximumIops", row.maximum_iops as f64),
            (
                "baselineThroughputInMBps",
                row.baseline_half_mbps as f64 / HALVES_PER_MBPS / MEGABITS_PER_MEGABYTE,
            ),
            (
                "maximumThroughputInMBps",
                row.maximum_mbps as f64 / MEGABITS_PER_MEGABYTE,
            ),
        ] {
            let actual = number(case, &performance, member);
            assert!(
                (actual - expected).abs() < 0.001,
                "case {case:?}: expected <{member}> {expected}, got {actual}"
            );
        }

        let (attachments, limit_type) = row.ebs_attachments;
        let actual = number(case, &ebs_info, "maximumEbsAttachments") as i64;
        assert_eq!(
            actual, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {actual}"
        );
        let actual = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            actual, limit_type,
            "case {case:?}: expected attachmentLimitType {limit_type:?}, got {actual:?}"
        );

        let storage = element(&body, "instanceStorageInfo");
        match row.instance_store {
            Some((devices, gb_each)) => {
                assert!(
                    storage.is_some(),
                    "case {case:?}: expected an instanceStorageInfo, got {body}"
                );
                for (path, expected) in [
                    (
                        vec!["instanceStorageInfo", "disks", "item", "count"],
                        devices,
                    ),
                    (
                        vec!["instanceStorageInfo", "disks", "item", "sizeInGB"],
                        gb_each,
                    ),
                    (
                        vec!["instanceStorageInfo", "totalSizeInGB"],
                        devices * gb_each,
                    ),
                ] {
                    let member = path.last().copied().unwrap_or_default();
                    let actual = reported_instance_type_member(case, &body, &path)
                        .unwrap_or_else(|| panic!("case {case:?}: no <{member}> in {body}"));
                    assert_eq!(
                        actual,
                        expected.to_string(),
                        "case {case:?}: expected {member} {expected}, got {actual:?}"
                    );
                }
            }
            None => assert!(
                storage.is_none(),
                "case {case:?}: a type with no local devices reports no instanceStorageInfo, \
                 got {storage:?}"
            ),
        }

        let network_info = required(case, &body, "networkInfo");
        let actual = required(case, &network_info, "networkPerformance");
        assert_eq!(
            actual, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {actual:?}",
            row.network_performance
        );

        let processor_info = required(case, &body, "processorInfo");
        let (manufacturer, sustained_ghz) = row.processor;
        let actual = required(case, &processor_info, "manufacturer");
        assert_eq!(
            actual, manufacturer,
            "case {case:?}: expected processor manufacturer {manufacturer:?}, got {actual:?}"
        );
        let actual = number(case, &processor_info, "sustainedClockSpeedInGhz");
        assert!(
            (actual - sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {sustained_ghz}, got {actual}"
        );
    }
}

/// Rule: the adapter capabilities, interface limits and host features a type is offered with
/// are published per type, so a hardware generation does not answer for every family on it and
/// a size ladder does not answer for every size of one family.
#[tokio::test]
async fn adapter_and_host_features_of_three_more_families_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // The network members, each with one type that has the capability beside one that does not:
    // the storage family offers ENA Express only from the size that carries a dedicated network
    // share, the AMD family is offered none of the three members its successors introduced, and
    // the network optimized family takes a configurable number of ENA queues at every size
    // while offering ENA Express only from the size that carries its whole dedicated share.
    let network_rows = [
        ("is4gen.medium", "enaSrdSupported", "false"),
        ("is4gen.2xlarge", "enaSrdSupported", "false"),
        ("is4gen.4xlarge", "enaSrdSupported", "false"),
        ("is4gen.8xlarge", "enaSrdSupported", "false"),
        ("m5a.large", "enaSrdSupported", "false"),
        ("m5a.24xlarge", "enaSrdSupported", "false"),
        ("m6in.4xlarge", "enaSrdSupported", "false"),
        ("m6in.8xlarge", "enaSrdSupported", "true"),
        ("m6in.metal", "enaSrdSupported", "true"),
        ("is4gen.8xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("m5a.12xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("m6in.large", "flexibleEnaQueuesSupport", "supported"),
        ("m6in.32xlarge", "flexibleEnaQueuesSupport", "supported"),
        ("m6in.metal", "flexibleEnaQueuesSupport", "unsupported"),
        ("is4gen.8xlarge", "secondaryNetworkSupported", "false"),
        ("m5a.12xlarge", "secondaryNetworkSupported", "false"),
        ("m6in.32xlarge", "secondaryNetworkSupported", "false"),
        ("is4gen.medium", "ipv4AddressesPerSecondaryInterface", "0"),
        ("m6in.large", "ipv4AddressesPerSecondaryInterface", "0"),
        ("m6in.metal", "ipv4AddressesPerSecondaryInterface", "0"),
        ("is4gen.8xlarge", "efaSupported", "false"),
        ("m5a.24xlarge", "efaSupported", "false"),
        ("m6in.16xlarge", "efaSupported", "false"),
        ("m6in.24xlarge", "efaSupported", "false"),
        ("m6in.32xlarge", "efaSupported", "true"),
        ("m6in.metal", "efaSupported", "true"),
        // The smallest size of the storage family takes two interfaces of four private
        // addresses, where its step of the size ladder publishes three of six; the two widest
        // sizes of the network optimized family split their interface limit over two cards.
        ("is4gen.medium", "maximumNetworkInterfaces", "2"),
        ("is4gen.medium", "ipv4AddressesPerInterface", "4"),
        ("is4gen.large", "maximumNetworkInterfaces", "3"),
        ("is4gen.large", "ipv4AddressesPerInterface", "10"),
        ("m6in.24xlarge", "maximumNetworkInterfaces", "15"),
        ("m6in.32xlarge", "maximumNetworkInterfaces", "16"),
        ("m6in.metal", "maximumNetworkInterfaces", "16"),
    ];

    for (instance_type, member, expected) in network_rows {
        let case = format!("{instance_type} {member}");
        let body = described_instance_type(&case, &router, REGION, instance_type).await;
        let network_info = required(&case, &body, "networkInfo");
        let actual = required(&case, &network_info, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?}, got {actual:?}"
        );
    }

    // The host features, each a flag a client branches on: hibernation up to the memory the
    // prerequisites bound an instance at, an enclave, a trusted platform module, a Dedicated
    // Host, a PTP hardware clock, reboot migration, action based recovery and the burst model
    // of the type's vCPUs.
    let feature_rows = [
        ("is4gen.medium", "hibernationSupported", "true"),
        ("is4gen.4xlarge", "hibernationSupported", "true"),
        ("is4gen.8xlarge", "hibernationSupported", "false"),
        ("m5a.large", "hibernationSupported", "true"),
        ("m6in.large", "hibernationSupported", "true"),
        ("is4gen.xlarge", "nitroEnclavesSupport", "unsupported"),
        ("is4gen.8xlarge", "nitroEnclavesSupport", "unsupported"),
        ("m5a.xlarge", "nitroEnclavesSupport", "supported"),
        ("m6in.xlarge", "nitroEnclavesSupport", "supported"),
        ("is4gen.large", "nitroTpmSupport", "unsupported"),
        ("is4gen.8xlarge", "nitroTpmSupport", "unsupported"),
        ("m5a.large", "nitroTpmSupport", "supported"),
        ("m6in.large", "nitroTpmSupport", "supported"),
        ("m6in.metal", "nitroTpmSupport", "unsupported"),
        ("is4gen.large", "dedicatedHostsSupported", "false"),
        ("is4gen.8xlarge", "dedicatedHostsSupported", "false"),
        ("m5a.12xlarge", "dedicatedHostsSupported", "false"),
        ("m6in.12xlarge", "dedicatedHostsSupported", "true"),
        ("is4gen.large", "phcSupport", "unsupported"),
        ("m5a.12xlarge", "phcSupport", "unsupported"),
        ("m6in.32xlarge", "phcSupport", "unsupported"),
        ("is4gen.large", "rebootMigrationSupport", "unsupported"),
        ("is4gen.8xlarge", "rebootMigrationSupport", "unsupported"),
        ("m5a.24xlarge", "rebootMigrationSupport", "supported"),
        ("m6in.large", "rebootMigrationSupport", "supported"),
        ("m6in.metal", "rebootMigrationSupport", "supported"),
        ("is4gen.large", "autoRecoverySupported", "false"),
        ("m5a.large", "autoRecoverySupported", "true"),
        ("m6in.32xlarge", "autoRecoverySupported", "true"),
        ("m6in.metal", "autoRecoverySupported", "true"),
        ("is4gen.large", "cpuBurstModel", "none"),
        ("m5a.large", "cpuBurstModel", "none"),
        ("m6in.large", "cpuBurstModel", "none"),
    ];

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described_instance_type(&case, &router, REGION, instance_type).await;
        let actual = required(&case, &body, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?}, got {actual:?}"
        );
    }

    // The bare metal size of the network optimized family boots in the legacy BIOS mode alone,
    // where its virtualized sizes take either mode.
    for (instance_type, expected) in [
        ("m6in.metal", vec!["legacy-bios"]),
        ("m6in.32xlarge", vec!["legacy-bios", "uefi"]),
        ("is4gen.8xlarge", vec!["uefi"]),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let actual = reported_instance_type_list(case, &body, &["supportedBootModes"]);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {actual:?}"
        );
    }
}

/// Rule: a network card reports the figures it CARRIES and the text of those same figures, so
/// the two readings of one card cannot disagree: every card of a type the catalog publishes a
/// network figure for carries that whole figure, whether the type carries one card or two.
#[tokio::test]
async fn a_network_card_reports_the_figure_it_carries_in_both_readings() {
    let (router, _engine) = super::ec2_query::emulator();

    // (type, cards, the instance's figure, ONE card's figure, what it sustains and reaches, its
    // interfaces). The rows span both halves of the rule: the two widest sizes of the network
    // optimized family carry two cards each holding the instance's whole published bandwidth and
    // half its interface limit, while the size below them and the smallest size of the same
    // family carry one card, which holds that same published figure whole.
    let rows = [
        (
            "m6in.32xlarge",
            2,
            "200 Gigabit",
            "200 Gigabit",
            200.0,
            200.0,
            8,
        ),
        ("m6in.metal", 2, "200 Gigabit", "200 Gigabit", 200.0, 200.0, 8),
        (
            "m6in.16xlarge",
            1,
            "100 Gigabit",
            "100 Gigabit",
            100.0,
            100.0,
            15,
        ),
        (
            "m6in.large",
            1,
            "Up to 25 Gigabit",
            "Up to 25 Gigabit",
            3.125,
            25.0,
            3,
        ),
    ];

    for (
        instance_type,
        cards,
        instance_figure,
        card_figure,
        card_baseline,
        card_peak,
        interfaces_per_card,
    ) in rows
    {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let actual = required(case, &network_info, "networkPerformance");
        assert_eq!(
            actual, instance_figure,
            "case {case:?}: expected networkPerformance {instance_figure:?}, got {actual:?}"
        );
        let actual = number(case, &network_info, "maximumNetworkCards") as i64;
        assert_eq!(
            actual, cards,
            "case {case:?}: expected {cards} network cards, got {actual}"
        );

        let parsed = document(case, &body);
        let node = parsed
            .descendants()
            .find(|inner| inner.is_element() && inner.tag_name().name() == "networkInfo")
            .unwrap_or_else(|| panic!("case {case:?}: no networkInfo node in {body}"));
        let reported = set_items(node, "networkCards");
        assert_eq!(
            reported.len() as i64,
            cards,
            "case {case:?}: expected one networkCards entry per card, got {body}"
        );
        for (index, card) in reported.iter().enumerate() {
            assert_eq!(
                child_text(*card, "networkCardIndex"),
                Some(index.to_string()),
                "case {case:?}: card {index} must report its own index, got {body}"
            );
            let actual = child_text(*card, "networkPerformance").unwrap_or_default();
            assert_eq!(
                actual, card_figure,
                "case {case:?}: expected card {index} networkPerformance {card_figure:?}, got \
                 {actual:?}"
            );
            for (member, expected) in [
                ("baselineBandwidthInGbps", card_baseline),
                ("peakBandwidthInGbps", card_peak),
            ] {
                let text = child_text(*card, member).unwrap_or_default();
                let actual: f64 = text.parse().unwrap_or_else(|_| {
                    panic!("case {case:?}: card {index} <{member}> must be a number, got {text:?}")
                });
                assert!(
                    (actual - expected).abs() < 0.001,
                    "case {case:?}: expected card {index} {member} {expected}, got {actual}"
                );
            }
            let interfaces = child_text(*card, "maximumNetworkInterfaces").unwrap_or_default();
            assert_eq!(
                interfaces,
                interfaces_per_card.to_string(),
                "case {case:?}: expected card {index} to take {interfaces_per_card} interfaces, \
                 got {interfaces:?}"
            );
        }
    }

    // The smallest size of the network optimized family sustains a share of its host that is
    // not a whole number of Mbps, which its one card reports in Gbps and the instance reports
    // through the two EBS readings of ONE figure.
    let body = described_instance_type("m6in.large", &router, REGION, "m6in.large").await;
    let performance = required(
        "m6in.large",
        &required("m6in.large", &body, "ebsInfo"),
        "ebsOptimizedInfo",
    );
    let whole_mbps = number("m6in.large", &performance, "baselineBandwidthInMbps");
    let megabytes = number("m6in.large", &performance, "baselineThroughputInMBps");
    assert!(
        megabytes * MEGABITS_PER_MEGABYTE > whole_mbps,
        "case \"m6in.large\": the throughput member carries the half of an Mbps the integer \
         bandwidth member cannot, got {whole_mbps} Mbps beside {megabytes} MB/s"
    );
}

/// Rule, the sharp edge: the published rows are NOT a function of the vCPUs the name gives. Two
/// families at ONE size disagree on every figure, and the core counts a caller may configure
/// are the published list rather than every count up to the default.
#[tokio::test]
async fn the_published_rows_of_three_more_families_are_not_a_function_of_the_size() {
    let (router, _engine) = super::ec2_query::emulator();

    let ebs_pair = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumBandwidthInMbps") as i64,
        )
    };

    // Two families, ONE size — 48 vCPUs each — disagreeing on the EBS figures and on the
    // network performance.
    let left = described_instance_type("m5a.12xlarge", &router, REGION, "m5a.12xlarge").await;
    let right = described_instance_type("m6in.12xlarge", &router, REGION, "m6in.12xlarge").await;
    for (case, body) in [("m5a.12xlarge", &left), ("m6in.12xlarge", &right)] {
        let vcpus = number(case, body, "defaultVCpus") as i64;
        assert_eq!(
            vcpus, 48,
            "case {case:?}: both rows must be the same size, got {vcpus} vCPUs"
        );
    }
    assert_ne!(
        ebs_pair(&left, "m5a.12xlarge"),
        ebs_pair(&right, "m6in.12xlarge"),
        "two families of one size publish different EBS figures"
    );
    assert_ne!(
        required(
            "m5a.12xlarge",
            &required("m5a.12xlarge", &left, "networkInfo"),
            "networkPerformance"
        ),
        required(
            "m6in.12xlarge",
            &required("m6in.12xlarge", &right, "networkInfo"),
            "networkPerformance"
        ),
        "two families of one size publish different network performance"
    );

    // The AMD family's sustained figure rises to its ceiling from the middle of its ladder up,
    // while the network optimized family's smallest sizes stay well under theirs: the relation
    // is published per type, not derived.
    for (instance_type, sustains_its_ceiling) in [
        ("m5a.xlarge", false),
        ("m5a.12xlarge", true),
        ("m5a.24xlarge", true),
        ("m6in.large", false),
        ("m6in.4xlarge", false),
        ("m6in.8xlarge", true),
        ("is4gen.large", false),
        ("is4gen.4xlarge", true),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let (baseline, maximum) = ebs_pair(&body, case);
        assert_eq!(
            baseline == maximum,
            sustains_its_ceiling,
            "case {case:?}: expected a size that {} its ceiling, got {baseline}/{maximum}",
            if sustains_its_ceiling {
                "sustains"
            } else {
                "bursts above"
            }
        );
    }

    // The core counts a caller may configure: an EPYC takes its cores two at a time, so the
    // two-core size publishes the one count two and each wider size the even counts from the
    // smallest configuration it publishes; the Intel family publishes every count up to its
    // default until its two widest sizes, which take them two at a time from four; a Graviton
    // size publishes every count, each of its vCPUs being a core of its own; and a size that
    // takes its cores all at once publishes the one count its default is.
    let even_from = |first: i64, cores: i64| -> Vec<i64> {
        (first..=cores).filter(|core| core % 2 == 0).collect()
    };
    for (instance_type, expected) in [
        ("m5a.large", vec![1_i64]),
        ("m5a.xlarge", vec![2_i64]),
        ("m5a.4xlarge", even_from(2, 8)),
        ("m5a.8xlarge", even_from(4, 16)),
        ("m5a.12xlarge", vec![6_i64, 12, 18, 24]),
        ("m5a.16xlarge", even_from(8, 32)),
        ("m5a.24xlarge", vec![48_i64]),
        ("m6in.2xlarge", (1..=4).collect()),
        ("m6in.12xlarge", (1..=24).collect()),
        (
            "m6in.32xlarge",
            (4..=64).filter(|core| core % 2 == 0).collect(),
        ),
        ("m6in.metal", (1..=64).collect()),
        ("is4gen.medium", vec![1_i64]),
        ("is4gen.8xlarge", (1..=32).collect()),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let actual: Vec<i64> = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"])
            .iter()
            .map(|core| {
                core.parse()
                    .unwrap_or_else(|_| panic!("case {case:?}: a core count, got {core:?}"))
            })
            .collect();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected validCores {expected:?}, got {actual:?}"
        );
    }
}
