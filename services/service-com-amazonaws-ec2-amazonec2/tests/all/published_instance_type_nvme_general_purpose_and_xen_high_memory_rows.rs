// SPDX-License-Identifier: CC0-1.0

//! The published rows of two more instance-type families and a pair of previous-era ones — the
//! general purpose family that carries local NVMe devices on the first generation of the AWS
//! Nitro System, and the two high-memory families on the Xen hypervisor — driven over the wire
//! this service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with
//! XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one `published_instance_type_hardware_facts` pins: AN INSTANCE
//! TYPE'S HARDWARE AND CAPABILITIES ARE PUBLISHED FACTS OF THAT TYPE, read off a per-type
//! table, not computed from the size suffix. So a family can carry local devices whose COUNT
//! and SIZE do not follow one ratio up the ladder, an EBS-optimized mode whose single figure is
//! both its baseline and its maximum, an attachment limit its hypervisor sets rather than the
//! Nitro pool, interface limits one step below the step of the size ladder they sit on, action
//! based recovery on a type that has local devices, no reboot migration on a type whose
//! hypervisor could do it, and core counts taken one per processor socket — none of which any
//! function of the size multiplier produces.
//!
//! Every table below spans what the rule generalises over: three families of two different
//! designs, the smallest, a middle, the largest virtualized and a bare metal size of each where
//! the family has one, and — for each capability member — one type that has it beside one that
//! does not. The sizes the trace that filed this happened to name are rows among many, and the
//! closing test holds two families at ONE size against each other, so no row of these tables
//! can be read as the specification.

use super::ec2_query::{
    assert_published_ebs, described_instance_type, element, number, reported_instance_type_list,
    reported_instance_type_member, required,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The published hardware figures of one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `m5d.4xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (f64, f64, f64, f64),
    /// The EBS volumes it can hold, and whether that limit is shared with its other devices or
    /// is the EBS devices' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its processor's sustained frequency, in GHz.
    sustained_ghz: f64,
}

/// Rule: the EBS, instance store, network and processor figures of these families are the ones
/// AWS publishes for each type.
#[tokio::test]
async fn hardware_rows_of_the_nvme_and_xen_high_memory_families_are_published_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: five sizes of the general purpose NVMe family — whose EBS baseline bursts above
    // itself below the `4xlarge` and is dedicated from it up, and whose local store goes from
    // one device to two and four while the device size itself goes back DOWN; both sizes of the
    // high-memory family on the Xen hypervisor; and three sizes of its larger-memory sibling,
    // whose store stays ONE device of a published size per row until the widest size.
    let rows = [
        Published {
            instance_type: "m5d.large",
            ebs: (650.0, 3_600.0, 4_750.0, 18_750.0),
            ebs_attachments: (26, "shared"),
            instance_store: Some((1, 75)),
            network_performance: "Up to 10 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5d.4xlarge",
            ebs: (4_750.0, 18_750.0, 4_750.0, 18_750.0),
            ebs_attachments: (25, "shared"),
            instance_store: Some((2, 300)),
            network_performance: "Up to 10 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5d.12xlarge",
            ebs: (9_500.0, 40_000.0, 9_500.0, 40_000.0),
            ebs_attachments: (25, "shared"),
            instance_store: Some((2, 900)),
            network_performance: "12 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5d.16xlarge",
            ebs: (13_600.0, 60_000.0, 13_600.0, 60_000.0),
            ebs_attachments: (23, "shared"),
            instance_store: Some((4, 600)),
            network_performance: "20 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5d.metal",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            ebs_attachments: (31, "shared"),
            instance_store: Some((4, 900)),
            network_performance: "25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "x1.16xlarge",
            ebs: (7_000.0, 40_000.0, 7_000.0, 40_000.0),
            ebs_attachments: (40, "shared"),
            instance_store: Some((1, 1_920)),
            network_performance: "10 Gigabit",
            sustained_ghz: 2.3,
        },
        Published {
            instance_type: "x1.32xlarge",
            ebs: (14_000.0, 80_000.0, 14_000.0, 80_000.0),
            ebs_attachments: (40, "shared"),
            instance_store: Some((2, 1_920)),
            network_performance: "25 Gigabit",
            sustained_ghz: 2.3,
        },
        Published {
            instance_type: "x1e.xlarge",
            ebs: (500.0, 3_700.0, 500.0, 3_700.0),
            ebs_attachments: (40, "shared"),
            instance_store: Some((1, 120)),
            network_performance: "Up to 10 Gigabit",
            sustained_ghz: 2.3,
        },
        Published {
            instance_type: "x1e.8xlarge",
            ebs: (3_500.0, 20_000.0, 3_500.0, 20_000.0),
            ebs_attachments: (40, "shared"),
            instance_store: Some((1, 960)),
            network_performance: "Up to 10 Gigabit",
            sustained_ghz: 2.3,
        },
        Published {
            instance_type: "x1e.32xlarge",
            ebs: (14_000.0, 80_000.0, 14_000.0, 80_000.0),
            ebs_attachments: (40, "shared"),
            instance_store: Some((2, 1_920)),
            network_performance: "25 Gigabit",
            sustained_ghz: 2.3,
        },
    ];

    for row in rows {
        let case = row.instance_type;
        let body = described_instance_type(case, &router, REGION, row.instance_type).await;
        assert_published_ebs(case, &body, row.ebs);

        let ebs_info = required(case, &body, "ebsInfo");
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
        let reported = number(case, &processor_info, "sustainedClockSpeedInGhz");
        assert!(
            (reported - row.sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {}, got {reported}",
            row.sustained_ghz
        );
    }
}

/// Rule: the adapter capabilities, the interface limits and the host features a type is offered
/// with are published per type, so neither a hardware generation nor a step of the size ladder
/// answers for every family on it.
#[tokio::test]
async fn adapter_interface_and_host_features_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // The network members, each with one type that has the capability beside one that does not:
    // the Nitro family's adapter carries no ENA Express and no configurable queue count, and
    // the packets of a session are copied under the guest on both designs — where the older
    // storage optimized families on the same hypervisor publish no mirroring at all.
    let network_rows = [
        ("m5d.large", "enaSrdSupported", "false"),
        ("m5d.24xlarge", "enaSrdSupported", "false"),
        ("x1.16xlarge", "enaSrdSupported", "false"),
        ("x1e.32xlarge", "enaSrdSupported", "false"),
        ("m5d.large", "flexibleEnaQueuesSupport", "unsupported"),
        ("x1e.xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("m5d.large", "trafficMirroringSupport", "supported"),
        ("m5d.metal", "trafficMirroringSupport", "supported"),
        ("x1.16xlarge", "trafficMirroringSupport", "supported"),
        ("x1e.32xlarge", "trafficMirroringSupport", "supported"),
        ("h1.2xlarge", "trafficMirroringSupport", "unsupported"),
        ("i2.8xlarge", "trafficMirroringSupport", "unsupported"),
        ("m5d.large", "maximumNetworkCards", "1"),
        ("x1.32xlarge", "maximumNetworkCards", "1"),
        ("m5d.large", "defaultNetworkCardIndex", "0"),
        ("x1e.xlarge", "defaultNetworkCardIndex", "0"),
        // The interface limits: the Nitro family's are the step of the size ladder it sits on,
        // while both Xen families publish limits of their own — one step BELOW that ladder at
        // every size but the one where the two agree.
        ("m5d.large", "maximumNetworkInterfaces", "3"),
        ("m5d.4xlarge", "maximumNetworkInterfaces", "8"),
        ("m5d.16xlarge", "maximumNetworkInterfaces", "15"),
        ("x1.16xlarge", "maximumNetworkInterfaces", "8"),
        ("x1.32xlarge", "maximumNetworkInterfaces", "8"),
        ("x1e.xlarge", "maximumNetworkInterfaces", "3"),
        ("x1e.2xlarge", "maximumNetworkInterfaces", "4"),
        ("x1e.8xlarge", "maximumNetworkInterfaces", "4"),
        ("x1e.32xlarge", "maximumNetworkInterfaces", "8"),
        ("m5d.4xlarge", "ipv4AddressesPerInterface", "30"),
        ("x1.16xlarge", "ipv4AddressesPerInterface", "30"),
        ("x1e.xlarge", "ipv4AddressesPerInterface", "10"),
        ("x1e.2xlarge", "ipv4AddressesPerInterface", "15"),
        ("x1e.8xlarge", "ipv4AddressesPerInterface", "15"),
        ("x1e.32xlarge", "ipv4AddressesPerInterface", "30"),
        // An interface of a type offered IPv6 takes as many IPv6 addresses as it takes private
        // IPv4 ones, so the published figure answers both members.
        ("x1e.xlarge", "ipv6AddressesPerInterface", "10"),
        ("x1e.8xlarge", "ipv6AddressesPerInterface", "15"),
        ("x1.32xlarge", "ipv6AddressesPerInterface", "30"),
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

    // The host features, each a flag a client branches on: EBS optimization, action based
    // recovery, reboot migration, a PTP hardware clock and the burst model of the vCPUs.
    let feature_rows = [
        ("m5d.large", "autoRecoverySupported", "false"),
        ("m5d.metal", "autoRecoverySupported", "false"),
        ("x1.16xlarge", "autoRecoverySupported", "true"),
        ("x1e.xlarge", "autoRecoverySupported", "true"),
        ("x1e.32xlarge", "autoRecoverySupported", "true"),
        ("m5d.large", "rebootMigrationSupport", "unsupported"),
        ("m5d.12xlarge", "rebootMigrationSupport", "unsupported"),
        ("m5d.metal", "rebootMigrationSupport", "unsupported"),
        ("x1e.32xlarge", "rebootMigrationSupport", "unsupported"),
        ("m5d.large", "phcSupport", "unsupported"),
        ("x1.32xlarge", "phcSupport", "unsupported"),
        ("m5d.large", "cpuBurstModel", "none"),
        ("x1e.xlarge", "cpuBurstModel", "none"),
        ("m5d.16xlarge", "dedicatedHostsSupported", "true"),
        ("x1.16xlarge", "dedicatedHostsSupported", "true"),
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

    // Whether the type HAS an EBS-optimized mode, and the one network card it carries: the
    // Xen families' mode is on at every size, and their one card carries the whole instance's
    // published network figure, while the Nitro family's card also publishes the ENA queues
    // each of its interfaces is given.
    for (instance_type, ebs_optimized, card_baseline, queues) in [
        ("m5d.large", "default", 0.75, Some(8.0)),
        ("m5d.12xlarge", "default", 12.0, Some(8.0)),
        ("m5d.metal", "default", 25.0, Some(8.0)),
        ("x1.16xlarge", "default", 10.0, None),
        ("x1.32xlarge", "default", 25.0, None),
        ("x1e.32xlarge", "default", 25.0, None),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let reported = reported_instance_type_member(case, &body, &["ebsInfo", "ebsOptimizedSupport"]);
        assert_eq!(
            reported.as_deref(),
            Some(ebs_optimized),
            "case {case:?}: expected ebsOptimizedSupport {ebs_optimized:?}, got {reported:?}"
        );
        let member = &["networkInfo", "networkCards", "item", "baselineBandwidthInGbps"];
        let reported = reported_instance_type_member(case, &body, member)
            .unwrap_or_else(|| panic!("case {case:?}: the one network card reports its baseline"));
        let reported: f64 = reported
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: a bandwidth, got {reported:?}"));
        assert!(
            (reported - card_baseline).abs() < 0.001,
            "case {case:?}: expected the card's baselineBandwidthInGbps {card_baseline}, got \
             {reported}"
        );
        let member = &[
            "networkInfo",
            "networkCards",
            "item",
            "defaultEnaQueueCountPerInterface",
        ];
        let reported = reported_instance_type_member(case, &body, member).map(|queues| {
            queues
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("case {case:?}: a queue count, got {queues:?}"))
        });
        assert_eq!(
            reported, queues,
            "case {case:?}: expected the card's defaultEnaQueueCountPerInterface {queues:?}, \
             got {reported:?}"
        );
    }
}

/// Rule, the sharp edge: the published rows are NOT a function of the vCPUs the name gives. Two
/// families at ONE size disagree on every figure, one family's local devices change count
/// without changing size, the root device types a type's AMIs may be backed by are its own, and
/// the core counts a caller may configure are the published list rather than every count up to
/// the default.
#[tokio::test]
async fn the_published_rows_are_not_a_function_of_the_size() {
    let (router, _engine) = super::ec2_query::emulator();

    let ebs_pair = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumIops") as i64,
        )
    };

    // Two families, ONE size — 64 vCPUs each — disagreeing on the EBS figures, the network
    // performance, the attachment limit and the geometry of their local devices.
    let left = described_instance_type("m5d.16xlarge", &router, REGION, "m5d.16xlarge").await;
    let right = described_instance_type("x1.16xlarge", &router, REGION, "x1.16xlarge").await;
    for (case, body) in [("m5d.16xlarge", &left), ("x1.16xlarge", &right)] {
        let vcpus = number(case, body, "defaultVCpus") as i64;
        assert_eq!(
            vcpus, 64,
            "case {case:?}: both rows must be the same size, got {vcpus} vCPUs"
        );
    }
    assert_ne!(
        ebs_pair(&left, "m5d.16xlarge"),
        ebs_pair(&right, "x1.16xlarge"),
        "two families of one size publish different EBS figures"
    );
    for (member, left_value, right_value) in [
        (
            "maximumEbsAttachments",
            required("m5d.16xlarge", &required("m5d.16xlarge", &left, "ebsInfo"), "maximumEbsAttachments"),
            required("x1.16xlarge", &required("x1.16xlarge", &right, "ebsInfo"), "maximumEbsAttachments"),
        ),
        (
            "networkPerformance",
            required(
                "m5d.16xlarge",
                &required("m5d.16xlarge", &left, "networkInfo"),
                "networkPerformance",
            ),
            required(
                "x1.16xlarge",
                &required("x1.16xlarge", &right, "networkInfo"),
                "networkPerformance",
            ),
        ),
    ] {
        assert_ne!(
            left_value, right_value,
            "two families of one size publish different {member}"
        );
    }

    // One family, three sizes: the device SIZE goes down where the count goes up, so neither
    // figure is the total divided by a ratio of the size.
    for (instance_type, expected) in [
        ("m5d.8xlarge", (2_i64, 600_i64)),
        ("m5d.12xlarge", (2, 900)),
        ("m5d.16xlarge", (4, 600)),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let storage = required(case, &body, "instanceStorageInfo");
        let disks = required(case, &storage, "disks");
        let reported = (
            number(case, &disks, "count") as i64,
            number(case, &disks, "sizeInGB") as i64,
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected (count, sizeInGB) {expected:?}, got {reported:?}"
        );
    }

    // The root device types: the older high-memory family's AMIs may be backed by its instance
    // store as well as by an EBS volume, while its larger-memory sibling's and the Nitro
    // family's may not — one published list per type, not one answer for every type that has
    // local devices.
    for (instance_type, expected) in [
        ("x1.16xlarge", vec!["ebs", "instance-store"]),
        ("x1.32xlarge", vec!["ebs", "instance-store"]),
        ("x1e.32xlarge", vec!["ebs"]),
        ("m5d.large", vec!["ebs"]),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let reported = reported_instance_type_list(case, &body, &["supportedRootDeviceTypes"]);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedRootDeviceTypes {expected:?}, got {reported:?}"
        );
    }

    // The core counts a caller may configure: every count up to the default on a size whose
    // cores sit on one socket, the published steps where they sit on two or four, and the one
    // count a size that takes its cores all at once is configured with.
    for (instance_type, expected) in [
        ("m5d.xlarge", vec![2_i64]),
        ("m5d.4xlarge", vec![2, 4, 6, 8]),
        ("m5d.24xlarge", vec![48]),
        ("m5d.metal", (1..=48).collect()),
        ("x1e.8xlarge", (1..=16).collect()),
        ("x1.16xlarge", (1..=16).map(|step| step * 2).collect()),
        ("x1.32xlarge", (1..=16).map(|step| step * 4).collect()),
        ("x1e.32xlarge", (1..=16).map(|step| step * 4).collect()),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let reported: Vec<i64> = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"])
            .iter()
            .map(|cores| {
                cores
                    .parse()
                    .unwrap_or_else(|_| panic!("case {case:?}: a core count, got {cores:?}"))
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected validCores {expected:?}, got {reported:?}"
        );
    }
}
