// SPDX-License-Identifier: CC0-1.0

//! The published rows of three more instance-type families — a network optimized Graviton
//! family, a 4th generation Intel family whose EBS volumes have a limit of their own, and a
//! storage optimized Graviton family with local NVMe — driven over the wire this service
//! speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML;
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one `published_instance_type_hardware_facts` pins: AN INSTANCE
//! TYPE'S HARDWARE AND CAPABILITIES ARE PUBLISHED FACTS OF THAT TYPE, read off a per-type
//! table, not computed from the vCPUs and memory its name gives. So a family can publish a
//! baseline its largest size never raises to its own ceiling, an EBS attachment limit of its
//! own, an adapter that answers `unsupported` where its hardware generation would answer
//! `supported`, and a virtual trusted platform module the published list of the types offered
//! one leaves it out of — none of which any function of the size multiplier produces.
//!
//! Every table below spans what the rule generalises over: three families of three different
//! designs, the smallest, a middle, the largest virtualized and a bare metal size of each, a
//! family with local devices and two without, and — for each capability member — one type that
//! has it beside one that does not. The sizes the trace that filed this happened to name are
//! rows among many, and the closing test holds two families at ONE size against each other, so
//! no row of these tables can be read as the specification.

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
    element(xml, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> reported, got {xml}"))
}

/// One member of a reported type as a number.
fn number(case: &str, xml: &str, name: &str) -> f64 {
    let text = required(case, xml, name);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: <{name}> must be a number, got {text:?}"))
}

/// The entries of a reported list member, each as its text.
fn list(case: &str, xml: &str, name: &str) -> Vec<String> {
    elements(&required(case, xml, name), "item")
}

/// The published hardware figures of one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `c7gn.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// The EBS volumes it can hold, and whether that limit is shared with its other devices
    /// or is the EBS devices' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its processor's maker and that processor's sustained frequency, in GHz.
    processor: (&'static str, f64),
}

/// Rule: the EBS, instance store, network and processor figures of these three families are
/// the ones AWS publishes for each type.
#[tokio::test]
async fn hardware_rows_of_three_more_families_are_the_published_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: four sizes of the network optimized Graviton family — whose baseline is its share
    // of a host that sustains less than its own ceiling, so baseline and maximum differ at
    // EVERY size, the largest included; four sizes of the Intel family, whose EBS volumes have
    // a limit of their own that grows in steps of its own; and three sizes of the storage
    // family, whose device size changes up the ladder while the count only grows at the top.
    let rows = [
        Published {
            instance_type: "c7gn.medium",
            ebs: (521, 2_083, 10_000, 40_000),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "Up to 25 Gigabit",
            processor: ("AWS", 2.6),
        },
        Published {
            instance_type: "c7gn.2xlarge",
            ebs: (4_167, 16_667, 10_000, 40_000),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "Up to 50 Gigabit",
            processor: ("AWS", 2.6),
        },
        Published {
            instance_type: "c7gn.12xlarge",
            ebs: (25_000, 100_000, 30_000, 120_000),
            ebs_attachments: (27, "shared"),
            instance_store: None,
            network_performance: "150 Gigabit",
            processor: ("AWS", 2.6),
        },
        Published {
            instance_type: "c7gn.metal",
            ebs: (33_333, 133_333, 40_000, 160_000),
            ebs_attachments: (31, "shared"),
            instance_store: None,
            network_performance: "200 Gigabit",
            processor: ("AWS", 2.6),
        },
        Published {
            instance_type: "c7i.large",
            ebs: (650, 3_600, 10_000, 40_000),
            ebs_attachments: (32, "dedicated"),
            instance_store: None,
            network_performance: "Up to 12.5 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "c7i.16xlarge",
            ebs: (20_000, 80_000, 20_000, 80_000),
            ebs_attachments: (48, "dedicated"),
            instance_store: None,
            network_performance: "25 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "c7i.48xlarge",
            ebs: (40_000, 240_000, 40_000, 240_000),
            ebs_attachments: (128, "dedicated"),
            instance_store: None,
            network_performance: "50 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "c7i.metal-48xl",
            ebs: (40_000, 240_000, 40_000, 240_000),
            ebs_attachments: (79, "dedicated"),
            instance_store: None,
            network_performance: "50 Gigabit",
            processor: ("Intel", 3.2),
        },
        Published {
            instance_type: "i4g.large",
            ebs: (625, 2_500, 10_000, 40_000),
            ebs_attachments: (26, "shared"),
            instance_store: Some((1, 468)),
            network_performance: "Up to 10 Gigabit",
            processor: ("AWS", 2.5),
        },
        Published {
            instance_type: "i4g.4xlarge",
            ebs: (5_000, 20_000, 10_000, 40_000),
            ebs_attachments: (26, "shared"),
            instance_store: Some((1, 3_750)),
            network_performance: "Up to 25 Gigabit",
            processor: ("AWS", 2.5),
        },
        Published {
            instance_type: "i4g.16xlarge",
            ebs: (20_000, 80_000, 20_000, 80_000),
            ebs_attachments: (23, "shared"),
            instance_store: Some((4, 3_750)),
            network_performance: "37.5 Gigabit",
            processor: ("AWS", 2.5),
        },
    ];

    for row in rows {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = row.ebs;
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
        // The throughput members are the bandwidth ones in the other unit, which is how the
        // published table's Mbps and MB/s columns relate.
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

/// Rule: the adapter capabilities and host features a type is offered with are published per
/// type, so a hardware generation does not answer for every family on it.
#[tokio::test]
async fn adapter_and_host_features_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: for each member, one type that has the capability beside one that does not. The
    // network optimized family answers `unsupported` to the three members its hardware
    // generation introduced, offers ENA Express only from the size that carries a dedicated
    // network share up, and is offered no enclave at any size, while the Intel family on the
    // same generation answers the opposite for most of them; the storage family's older
    // adapter encrypts in-transit traffic all the same, carries an Elastic Fabric Adapter on
    // the size that holds the whole host, and takes an enclave from two vCPUs up.
    let rows = [
        ("c7gn.medium", "enaSrdSupported", "false"),
        ("c7gn.2xlarge", "enaSrdSupported", "false"),
        ("c7gn.4xlarge", "enaSrdSupported", "true"),
        ("c7gn.metal", "enaSrdSupported", "true"),
        ("c7i.large", "enaSrdSupported", "false"),
        ("c7i.12xlarge", "enaSrdSupported", "true"),
        ("i4g.large", "enaSrdSupported", "false"),
        ("i4g.4xlarge", "enaSrdSupported", "true"),
        ("c7gn.12xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("c7i.48xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("c6in.16xlarge", "flexibleEnaQueuesSupport", "supported"),
        ("c7gn.16xlarge", "secondaryNetworkSupported", "false"),
        ("c7i.16xlarge", "secondaryNetworkSupported", "false"),
        ("c7gn.16xlarge", "ipv4AddressesPerSecondaryInterface", "0"),
        ("c7i.16xlarge", "ipv4AddressesPerSecondaryInterface", "0"),
        ("i4g.16xlarge", "ipv4AddressesPerSecondaryInterface", "0"),
        ("c7gn.xlarge", "encryptionInTransitSupported", "true"),
        ("i4g.large", "encryptionInTransitSupported", "true"),
        ("i4g.16xlarge", "encryptionInTransitSupported", "true"),
        ("c7i.large", "efaSupported", "false"),
        ("c7i.24xlarge", "efaSupported", "false"),
        ("c7i.48xlarge", "efaSupported", "true"),
        ("c7i.metal-48xl", "efaSupported", "true"),
        ("i4g.large", "efaSupported", "false"),
        ("i4g.16xlarge", "efaSupported", "true"),
        ("c7gn.12xlarge", "trafficMirroringSupport", "unsupported"),
        ("c7gn.metal", "trafficMirroringSupport", "unsupported"),
        ("c7i.12xlarge", "trafficMirroringSupport", "supported"),
        ("i4g.4xlarge", "trafficMirroringSupport", "supported"),
    ];

    for (instance_type, member, expected) in rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let network_info = required(&case, &body, "networkInfo");
        let reported = required(&case, &network_info, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The host features, each a flag a client branches on: an enclave, a trusted platform
    // module, a PTP hardware clock, reboot migration, action based recovery, hibernation,
    // Dedicated Hosts and the burst model of the type's vCPUs.
    let feature_rows = [
        ("c7gn.xlarge", "nitroEnclavesSupport", "unsupported"),
        ("c7gn.16xlarge", "nitroEnclavesSupport", "unsupported"),
        ("c7i.xlarge", "nitroEnclavesSupport", "supported"),
        ("i4g.large", "nitroEnclavesSupport", "supported"),
        ("i4g.16xlarge", "nitroEnclavesSupport", "supported"),
        ("i4g.large", "nitroTpmSupport", "unsupported"),
        ("i4g.16xlarge", "nitroTpmSupport", "unsupported"),
        ("c7gn.16xlarge", "nitroTpmSupport", "supported"),
        ("c7i.large", "nitroTpmSupport", "supported"),
        ("c7gn.metal", "nitroTpmSupport", "unsupported"),
        ("c7i.metal-24xl", "nitroTpmSupport", "unsupported"),
        ("c7i.metal-48xl", "nitroTpmSupport", "unsupported"),
        ("c7gn.12xlarge", "phcSupport", "unsupported"),
        ("c7gn.metal", "phcSupport", "unsupported"),
        ("i4g.8xlarge", "phcSupport", "unsupported"),
        ("c7i.metal-24xl", "phcSupport", "supported"),
        ("c7gn.12xlarge", "rebootMigrationSupport", "supported"),
        ("c7gn.metal", "rebootMigrationSupport", "supported"),
        ("i4g.large", "rebootMigrationSupport", "unsupported"),
        ("i4g.16xlarge", "rebootMigrationSupport", "unsupported"),
        ("c7gn.metal", "autoRecoverySupported", "true"),
        ("c7gn.12xlarge", "autoRecoverySupported", "true"),
        ("i4g.large", "autoRecoverySupported", "false"),
        ("i4g.large", "hibernationSupported", "true"),
        ("i4g.4xlarge", "hibernationSupported", "true"),
        ("i4g.8xlarge", "hibernationSupported", "false"),
        ("i4g.16xlarge", "hibernationSupported", "false"),
        ("c7gn.metal", "hibernationSupported", "false"),
        ("c7i.16xlarge", "dedicatedHostsSupported", "true"),
        ("c7i.metal-48xl", "dedicatedHostsSupported", "true"),
        ("c7i.24xlarge", "dedicatedHostsSupported", "false"),
        ("c7i.48xlarge", "dedicatedHostsSupported", "false"),
        ("c7i.metal-24xl", "dedicatedHostsSupported", "false"),
        ("c7gn.12xlarge", "dedicatedHostsSupported", "true"),
        ("c7gn.12xlarge", "cpuBurstModel", "none"),
        ("c7i.large", "cpuBurstModel", "none"),
        ("i4g.large", "cpuBurstModel", "none"),
    ];

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let reported = required(&case, &body, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // A type offered no trusted platform module describes none either: the flag and the
    // versions it would carry cannot disagree. The rows span both halves of the published
    // answer — two virtualized types the published list of the types offered NitroTPM leaves
    // out, two it names, and the bare metal sizes it leaves out whatever their family.
    for (instance_type, described_module) in [
        ("i4g.large", false),
        ("i4g.16xlarge", false),
        ("c7i.large", true),
        ("c7gn.16xlarge", true),
        ("c7gn.metal", false),
        ("c7i.metal-24xl", false),
        ("c7i.metal-48xl", false),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let info = element(&body, "nitroTpmInfo");
        assert_eq!(
            info.is_some(),
            described_module,
            "case {case:?}: a type offered a trusted platform module describes its versions \
             and one offered none describes nothing, got {info:?}"
        );
        if described_module {
            let versions = list(case, &body, "supportedVersions");
            assert!(
                !versions.is_empty(),
                "case {case:?}: expected the supported versions, got {versions:?}"
            );
        }
    }
}

/// Rule, the sharp edge: the published figures are NOT a function of the vCPUs and memory the
/// name gives. Two families at ONE size disagree on every figure, a family's baseline stays
/// under its own ceiling at the size that holds the whole host, and the core counts a caller
/// may configure are the published list rather than every count up to the default.
#[tokio::test]
async fn the_published_rows_are_not_a_function_of_the_size() {
    let (router, _engine) = super::ec2_query::emulator();

    let ebs_pair = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumBandwidthInMbps") as i64,
        )
    };

    // Two families, ONE size — 64 vCPUs each — disagreeing on the EBS figures, the network
    // performance and even the KIND of their EBS attachment limit.
    let left = described("c7gn.16xlarge", &router, "c7gn.16xlarge").await;
    let right = described("c7i.16xlarge", &router, "c7i.16xlarge").await;
    for (case, body) in [("c7gn.16xlarge", &left), ("c7i.16xlarge", &right)] {
        let vcpus = number(case, body, "defaultVCpus") as i64;
        assert_eq!(
            vcpus, 64,
            "case {case:?}: both rows must be the same size, got {vcpus} vCPUs"
        );
    }
    assert_ne!(
        ebs_pair(&left, "c7gn.16xlarge"),
        ebs_pair(&right, "c7i.16xlarge"),
        "two families of one size publish different EBS figures"
    );
    assert_ne!(
        required(
            "c7gn.16xlarge",
            &required("c7gn.16xlarge", &left, "networkInfo"),
            "networkPerformance"
        ),
        required(
            "c7i.16xlarge",
            &required("c7i.16xlarge", &right, "networkInfo"),
            "networkPerformance"
        ),
        "two families of one size publish different network performance"
    );
    assert_ne!(
        required(
            "c7gn.16xlarge",
            &required("c7gn.16xlarge", &left, "ebsInfo"),
            "attachmentLimitType"
        ),
        required(
            "c7i.16xlarge",
            &required("c7i.16xlarge", &right, "ebsInfo"),
            "attachmentLimitType"
        ),
        "two families of one size publish different kinds of EBS attachment limit"
    );

    // One family, four sizes: the baseline stays BELOW the ceiling at every one of them, the
    // size that holds the whole host included — which no "the largest size sustains its
    // maximum" rule produces.
    for instance_type in [
        "c7gn.medium",
        "c7gn.4xlarge",
        "c7gn.16xlarge",
        "c7gn.metal",
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let (baseline, maximum) = ebs_pair(&body, case);
        assert!(
            baseline < maximum,
            "case {case:?}: this family's baseline stays under its ceiling, got \
             {baseline}/{maximum}"
        );
    }
    // While the Intel family's largest size sustains its ceiling, which is the other half of
    // the same rule: the relation is published per family, not derived.
    let body = described("c7i.48xlarge", &router, "c7i.48xlarge").await;
    let (baseline, maximum) = ebs_pair(&body, "c7i.48xlarge");
    assert_eq!(
        baseline, maximum,
        "case \"c7i.48xlarge\": this family's largest size sustains its ceiling, got \
         {baseline}/{maximum}"
    );

    // The core counts a caller may configure: the size that holds the whole host publishes the
    // one count it takes its cores all at once in, while the size below it publishes every
    // count up to its default.
    for (instance_type, expected) in [
        ("c7i.48xlarge", vec![96_i64]),
        ("c7i.24xlarge", (1..=48).collect()),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let v_cpu_info = required(case, &body, "vCpuInfo");
        let reported: Vec<i64> = list(case, &v_cpu_info, "validCores")
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
