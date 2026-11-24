// SPDX-License-Identifier: CC0-1.0

//! The published rows of the three fifth generation network optimized instance-type families —
//! the general purpose one, the same host with local NVMe devices and the memory optimized one —
//! driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test is the one `published_instance_type_hardware_facts` pins: AN INSTANCE
//! TYPE'S HARDWARE AND CAPABILITIES ARE PUBLISHED FACTS OF THAT TYPE, read off a per-type table,
//! not computed from the size suffix. These three families are where that bites hardest: they
//! share one host with the families that carry no `n` capability letter, so their EBS figures and
//! local devices are that host's while their network share, their adapter's queue counts, their
//! Elastic Fabric Adapter and their hibernation answer are the letter's — and a letter is not a
//! size multiplier.
//!
//! Every table below spans what the rule generalises over: three families, the smallest, a
//! middle, the largest virtualized and the bare metal size of each, one size whose EBS baseline
//! bursts above itself beside one whose bandwidth is dedicated, one size offered an Elastic
//! Fabric Adapter beside one that is not, and a size with local devices beside one without. The
//! sizes the trace that filed this happened to name are rows among many, and the closing test
//! holds two families at ONE size against each other, so no row of these tables can be read as
//! the specification.

use super::ec2_query::{
    assert_published_ebs, described_instance_type, element, number, reported_instance_type_list,
    reported_instance_type_member, required,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The published hardware figures of one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `m5dn.8xlarge` and the like.
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
async fn hardware_rows_of_the_fifth_generation_network_optimized_families_are_published_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: five sizes of the general purpose family, six of the one that adds local NVMe
    // devices — whose device count goes to two and four while the device size itself goes back
    // DOWN — and five of the memory optimized one. The EBS baseline bursts above itself up to
    // the `4xlarge` and is dedicated from it, and the network share is the published one of each
    // size rather than its slice of the host.
    let rows = [
        Published {
            instance_type: "m5n.large",
            ebs: (650.0, 3_600.0, 4_750.0, 18_750.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "Up to 25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5n.4xlarge",
            ebs: (4_750.0, 18_750.0, 4_750.0, 18_750.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "Up to 25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5n.8xlarge",
            ebs: (6_800.0, 30_000.0, 6_800.0, 30_000.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5n.16xlarge",
            ebs: (13_600.0, 60_000.0, 13_600.0, 60_000.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "75 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5n.metal",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            ebs_attachments: (31, "shared"),
            instance_store: None,
            network_performance: "100 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5dn.large",
            ebs: (650.0, 3_600.0, 4_750.0, 18_750.0),
            ebs_attachments: (26, "shared"),
            instance_store: Some((1, 75)),
            network_performance: "Up to 25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5dn.4xlarge",
            ebs: (4_750.0, 18_750.0, 4_750.0, 18_750.0),
            ebs_attachments: (25, "shared"),
            instance_store: Some((2, 300)),
            network_performance: "Up to 25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5dn.8xlarge",
            ebs: (6_800.0, 30_000.0, 6_800.0, 30_000.0),
            ebs_attachments: (25, "shared"),
            instance_store: Some((2, 600)),
            network_performance: "25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5dn.12xlarge",
            ebs: (9_500.0, 40_000.0, 9_500.0, 40_000.0),
            ebs_attachments: (25, "shared"),
            instance_store: Some((2, 900)),
            network_performance: "50 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5dn.16xlarge",
            ebs: (13_600.0, 60_000.0, 13_600.0, 60_000.0),
            ebs_attachments: (23, "shared"),
            instance_store: Some((4, 600)),
            network_performance: "75 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "m5dn.metal",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            ebs_attachments: (31, "shared"),
            instance_store: Some((4, 900)),
            network_performance: "100 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "r5n.xlarge",
            ebs: (1_150.0, 6_000.0, 4_750.0, 18_750.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "Up to 25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "r5n.2xlarge",
            ebs: (2_300.0, 12_000.0, 4_750.0, 18_750.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "Up to 25 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "r5n.12xlarge",
            ebs: (9_500.0, 40_000.0, 9_500.0, 40_000.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "50 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "r5n.24xlarge",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "100 Gigabit",
            sustained_ghz: 3.1,
        },
        Published {
            instance_type: "r5n.metal",
            ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
            ebs_attachments: (31, "shared"),
            instance_store: None,
            network_performance: "100 Gigabit",
            sustained_ghz: 3.1,
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
async fn adapter_interface_and_host_features_of_the_network_optimized_families_are_published() {
    let (router, _engine) = super::ec2_query::emulator();

    // The network members, each with one type that has the capability beside one that does not:
    // the Elastic Fabric Adapter reaches only the sizes that hold a whole host — so the
    // `16xlarge` below them is offered none — while the generation's adapter carries neither ENA
    // Express nor a configurable queue count at any size, and the packets of a session are
    // copied under every virtualized guest but under no bare metal one.
    let network_rows = [
        ("m5n.16xlarge", "efaSupported", "false"),
        ("m5dn.16xlarge", "efaSupported", "false"),
        ("r5n.16xlarge", "efaSupported", "false"),
        ("m5n.24xlarge", "efaSupported", "true"),
        ("m5dn.metal", "efaSupported", "true"),
        ("r5n.24xlarge", "efaSupported", "true"),
        ("m5n.large", "enaSrdSupported", "false"),
        ("m5dn.24xlarge", "enaSrdSupported", "false"),
        ("r5n.metal", "enaSrdSupported", "false"),
        ("m5n.large", "flexibleEnaQueuesSupport", "unsupported"),
        ("m5dn.8xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("r5n.xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("m5n.large", "secondaryNetworkSupported", "false"),
        ("r5n.24xlarge", "secondaryNetworkSupported", "false"),
        ("m5dn.4xlarge", "trafficMirroringSupport", "supported"),
        ("r5n.xlarge", "trafficMirroringSupport", "supported"),
        ("m5n.metal", "trafficMirroringSupport", "unsupported"),
        ("r5n.metal", "trafficMirroringSupport", "unsupported"),
        ("m5n.large", "maximumNetworkCards", "1"),
        ("m5dn.metal", "maximumNetworkCards", "1"),
        ("m5n.large", "defaultNetworkCardIndex", "0"),
        ("r5n.metal", "defaultNetworkCardIndex", "0"),
        // The interface limits are the step of the size ladder these families sit on, which the
        // published rows leave alone — the figure a client reads beside the per-card one below.
        ("m5n.large", "maximumNetworkInterfaces", "3"),
        ("r5n.xlarge", "maximumNetworkInterfaces", "4"),
        ("m5n.4xlarge", "maximumNetworkInterfaces", "8"),
        ("m5dn.8xlarge", "maximumNetworkInterfaces", "8"),
        ("m5dn.16xlarge", "maximumNetworkInterfaces", "15"),
        ("r5n.xlarge", "ipv4AddressesPerInterface", "15"),
        ("m5n.4xlarge", "ipv4AddressesPerInterface", "30"),
        ("m5dn.16xlarge", "ipv4AddressesPerInterface", "50"),
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

    // The host features, each a flag a client branches on: On-Demand hibernation — which these
    // families are offered at NO size, however much memory the size has and whether or not the
    // family it shares its host with is offered it — action based recovery, which a type with
    // local devices does not get and a bare metal type with none does, reboot migration, a PTP
    // hardware clock and the burst model of the vCPUs.
    let feature_rows = [
        ("m5n.large", "hibernationSupported", "false"),
        ("m5n.4xlarge", "hibernationSupported", "false"),
        ("m5dn.large", "hibernationSupported", "false"),
        ("m5dn.8xlarge", "hibernationSupported", "false"),
        ("r5n.2xlarge", "hibernationSupported", "false"),
        ("r5n.4xlarge", "hibernationSupported", "false"),
        ("m5n.4xlarge", "autoRecoverySupported", "true"),
        ("m5n.metal", "autoRecoverySupported", "true"),
        ("r5n.metal", "autoRecoverySupported", "true"),
        ("m5dn.4xlarge", "autoRecoverySupported", "false"),
        ("m5dn.metal", "autoRecoverySupported", "false"),
        // A migration moves a running instance to another host, so the families with no local
        // devices are offered it at every size — the bare metal one included, where the move is
        // the host's to make — and the family whose devices hold data no move carries over is
        // offered it at none.
        ("m5n.large", "rebootMigrationSupport", "supported"),
        ("m5n.metal", "rebootMigrationSupport", "supported"),
        ("r5n.24xlarge", "rebootMigrationSupport", "supported"),
        ("r5n.metal", "rebootMigrationSupport", "supported"),
        ("m5dn.large", "rebootMigrationSupport", "unsupported"),
        ("m5dn.12xlarge", "rebootMigrationSupport", "unsupported"),
        ("m5dn.metal", "rebootMigrationSupport", "unsupported"),
        ("m5n.large", "phcSupport", "unsupported"),
        ("r5n.metal", "phcSupport", "unsupported"),
        ("m5n.large", "cpuBurstModel", "none"),
        ("m5dn.metal", "cpuBurstModel", "none"),
        ("m5n.16xlarge", "dedicatedHostsSupported", "true"),
        ("r5n.xlarge", "dedicatedHostsSupported", "true"),
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

    // The boot modes: the size that holds a whole host is offered the legacy BIOS one alone,
    // where every virtualized size of the same hardware takes either.
    for (instance_type, expected) in [
        ("m5n.metal", vec!["legacy-bios"]),
        ("m5dn.metal", vec!["legacy-bios"]),
        ("r5n.metal", vec!["legacy-bios"]),
        ("m5n.large", vec!["legacy-bios", "uefi"]),
        ("m5dn.24xlarge", vec!["legacy-bios", "uefi"]),
        ("r5n.xlarge", vec!["legacy-bios", "uefi"]),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let reported = reported_instance_type_list(case, &body, &["supportedBootModes"]);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, the sharp edge: the published rows are NOT a function of the size the name gives. The
/// one network card of a type carries the published figure of that type and the queues its
/// adapter gives one interface, the core counts a caller may configure are the published list
/// rather than every count up to the default, and two families at ONE size disagree.
#[tokio::test]
async fn the_network_optimized_rows_are_not_a_function_of_the_size() {
    let (router, _engine) = super::ec2_query::emulator();

    // The one card of each type: the published Gbps it sustains, the Gbps it reaches and the ENA
    // queues one of its interfaces is given — a ladder that is neither the size's slice of the
    // host nor one figure for the whole family.
    for (instance_type, baseline, peak, queues) in [
        ("m5n.large", 2.1, 25.0, 8.0),
        ("r5n.xlarge", 4.1, 25.0, 8.0),
        ("m5dn.2xlarge", 8.125, 25.0, 16.0),
        ("m5n.4xlarge", 16.25, 25.0, 32.0),
        ("m5dn.8xlarge", 25.0, 25.0, 32.0),
        ("r5n.12xlarge", 50.0, 50.0, 32.0),
        ("m5dn.16xlarge", 75.0, 75.0, 32.0),
        ("m5n.metal", 100.0, 100.0, 32.0),
    ] {
        let case = instance_type;
        let body = described_instance_type(case, &router, REGION, instance_type).await;
        let figure = |member: &str| -> f64 {
            let path = ["networkInfo", "networkCards", "item", member];
            let reported = reported_instance_type_member(case, &body, &path)
                .unwrap_or_else(|| panic!("case {case:?}: the one card reports its {member}"));
            reported
                .parse()
                .unwrap_or_else(|_| panic!("case {case:?}: a figure, got {reported:?}"))
        };
        for (member, expected) in [
            ("baselineBandwidthInGbps", baseline),
            ("peakBandwidthInGbps", peak),
            ("defaultEnaQueueCountPerInterface", queues),
        ] {
            let reported = figure(member);
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected the card's {member} {expected}, got {reported}"
            );
        }
    }

    // The core counts a caller may configure: every count up to the default on the narrow sizes,
    // the published steps of two where the cores sit on two sockets, the one count the largest
    // virtualized size takes its cores all at once in, and every count on the size that holds
    // the whole host.
    for (instance_type, expected) in [
        ("m5n.large", vec![1_i64]),
        ("m5n.xlarge", vec![1, 2]),
        ("r5n.xlarge", vec![1, 2]),
        ("m5n.2xlarge", vec![2, 4]),
        ("m5dn.4xlarge", vec![2, 4, 6, 8]),
        ("r5n.16xlarge", (1..=16).map(|step| step * 2).collect()),
        ("r5n.24xlarge", vec![48]),
        ("m5dn.24xlarge", vec![48]),
        ("m5n.metal", (1..=48).collect()),
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

    // Two families at ONE size — 32 vCPUs each — agreeing on the host they share and differing
    // on what their own letters buy: the memory of the size, the local devices it carries and
    // the EBS volumes it can hold beside them.
    let left = described_instance_type("m5n.8xlarge", &router, REGION, "m5n.8xlarge").await;
    let right = described_instance_type("m5dn.8xlarge", &router, REGION, "m5dn.8xlarge").await;
    let memory = described_instance_type("r5n.8xlarge", &router, REGION, "r5n.8xlarge").await;
    for (case, body) in [
        ("m5n.8xlarge", &left),
        ("m5dn.8xlarge", &right),
        ("r5n.8xlarge", &memory),
    ] {
        let vcpus = number(case, body, "defaultVCpus") as i64;
        assert_eq!(
            vcpus, 32,
            "case {case:?}: every row here must be the same size, got {vcpus} vCPUs"
        );
        let reported = required(case, &required(case, body, "networkInfo"), "networkPerformance");
        assert_eq!(
            reported, "25 Gigabit",
            "case {case:?}: one host's share at this size, got {reported:?}"
        );
    }
    let attachments = |case: &str, body: &str| -> String {
        required(case, &required(case, body, "ebsInfo"), "maximumEbsAttachments")
    };
    assert_ne!(
        attachments("m5n.8xlarge", &left),
        attachments("m5dn.8xlarge", &right),
        "the local devices of one family take attachments the other's volumes keep"
    );
    assert!(
        element(&left, "instanceStorageInfo").is_none()
            && element(&right, "instanceStorageInfo").is_some(),
        "only the family whose letter names local devices reports an instance store"
    );
    let memory_mib = |case: &str, body: &str| -> f64 {
        number(case, &required(case, body, "memoryInfo"), "sizeInMiB")
    };
    assert_ne!(
        memory_mib("m5n.8xlarge", &left) as i64,
        memory_mib("r5n.8xlarge", &memory) as i64,
        "the general purpose and memory optimized families differ in memory at one size"
    );
}
