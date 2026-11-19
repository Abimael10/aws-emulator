// SPDX-License-Identifier: CC0-1.0

//! What `DescribeInstanceTypes` reports for the instance types whose hardware the size
//! ladder in their NAME does not describe, driven over the wire the service speaks
//! (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: EVERY MEMBER OF A REPORTED INSTANCE TYPE IS THE FIGURE AWS PUBLISHES
//! FOR THAT TYPE, AND EVERY MEMBER AWS REPORTS FOR EVERY TYPE IS PRESENT WHATEVER THE ANSWER
//! IS. A type's vCPUs, cores, threads, memory, EBS performance, attachment limit, local
//! devices, network performance and the lists of what it is offered (usage classes, boot
//! modes, virtualization types, placement group strategies, processor features, configurable
//! core counts) are per-type facts; they are not a function of the size multiplier in the
//! name, and a member whose answer is "none" or "unsupported" is reported with that answer
//! rather than left out.
//!
//! The rows below span what the rule generalises over rather than one family: a family whose
//! sizes get a FLEXIBLE share of their host (so a size sustains half the bandwidth its
//! ladder's row publishes while bursting to that row's maximum, and its smallest size
//! sustains a figure that is not a whole number of Mbps), two previous generation families on
//! the Xen hypervisor (one thread per core, a magnetic instance store, memory in no modern
//! ratio, no IPv6), and a High Memory family whose two sizes are the same server with its
//! second thread per core on and off. Each is a row among many, not the specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    described_instance_type, owned, reported_instance_type_list, reported_instance_type_member,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-west-2";

/// The megabits in a megabyte, which is all that separates the `Mbps` members of
/// `EbsOptimizedInfo` from its `MB/s` ones.
const MEGABITS_PER_MEGABYTE: f64 = 8.0;

/// How close two `MB/s` figures have to be to count as the same figure.
const THROUGHPUT_TOLERANCE: f64 = 0.001;

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// The text of the member at `path` inside the one reported instance type of `body`, or
/// `None` when the type reports no such member.
fn reported(case: &str, body: &str, path: &[&str]) -> Option<String> {
    reported_instance_type_member(case, body, path)
}

/// The member at `path`, or a panic naming the case when the type reports none: an absent
/// member is exactly what a client reading it cannot act on.
fn required(case: &str, body: &str, path: &[&str]) -> String {
    reported(case, body, path)
        .unwrap_or_else(|| panic!("case {case:?}: no <{}> reported, got {body}", path.join("/")))
}

/// The member at `path` as a number.
fn number(case: &str, body: &str, path: &[&str]) -> f64 {
    let text = required(case, body, path);
    text.parse().unwrap_or_else(|_| {
        panic!("case {case:?}: <{}> must be a number, got {text:?}", path.join("/"))
    })
}

/// The member at `path` as a whole number.
fn whole(case: &str, body: &str, path: &[&str]) -> i64 {
    number(case, body, path) as i64
}

/// The entries of the list member at `path`, in the order the type reported them.
fn list(case: &str, body: &str, path: &[&str]) -> Vec<String> {
    reported_instance_type_list(case, body, path)
}

/// The hardware figures AWS publishes for one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `c7i-flex.12xlarge` and the like.
    instance_type: &'static str,
    /// Its EBS-optimized performance as (sustained Mbps as the integer member reports it,
    /// sustained MB/s, the Mbps it reaches), or `None` for a type with no EBS-optimized mode
    /// at all, which has no such performance to describe.
    ebs_bandwidth: Option<(i64, f64, i64)>,
    /// (the IOPS it sustains, the IOPS it reaches), read only where it has that mode.
    ebs_iops: (i64, i64),
    /// The EBS volumes it can hold attached, and whether that limit is its own or shared
    /// with its interfaces and instance store devices.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its memory, in MiB.
    memory_mib: i64,
    /// (vCPUs, cores, threads per core).
    vcpus: (i64, i64, i64),
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
}

/// The table every figure test below reads: four sizes of the flex family, the whole ladder
/// of two previous generation families, and both sizes of the High Memory family.
const PUBLISHED: [Published; 13] = [
    Published {
        instance_type: "c7i-flex.large",
        ebs_bandwidth: Some((312, 39.0625, 10_000)),
        ebs_iops: (2_500, 40_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        memory_mib: 4_096,
        vcpus: (2, 1, 2),
        network_performance: "Up to 12.5 Gigabit",
    },
    Published {
        instance_type: "c7i-flex.xlarge",
        ebs_bandwidth: Some((625, 78.125, 10_000)),
        ebs_iops: (3_600, 40_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        memory_mib: 8_192,
        vcpus: (4, 2, 2),
        network_performance: "Up to 12.5 Gigabit",
    },
    Published {
        instance_type: "c7i-flex.12xlarge",
        ebs_bandwidth: Some((7_500, 937.5, 15_000)),
        ebs_iops: (30_000, 60_000),
        ebs_attachments: (32, "dedicated"),
        instance_store: None,
        memory_mib: 98_304,
        vcpus: (48, 24, 2),
        network_performance: "Up to 18.75 Gigabit",
    },
    Published {
        instance_type: "c7i-flex.16xlarge",
        ebs_bandwidth: Some((10_000, 1_250.0, 20_000)),
        ebs_iops: (40_000, 80_000),
        ebs_attachments: (48, "dedicated"),
        instance_store: None,
        memory_mib: 131_072,
        vcpus: (64, 32, 2),
        network_performance: "Up to 25 Gigabit",
    },
    Published {
        instance_type: "m1.small",
        ebs_bandwidth: None,
        ebs_iops: (0, 0),
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 160)),
        memory_mib: 1_740,
        vcpus: (1, 1, 1),
        network_performance: "Low",
    },
    Published {
        instance_type: "m1.medium",
        ebs_bandwidth: None,
        ebs_iops: (0, 0),
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 410)),
        memory_mib: 3_788,
        vcpus: (1, 1, 1),
        network_performance: "Moderate",
    },
    Published {
        instance_type: "m1.large",
        ebs_bandwidth: Some((500, 62.5, 500)),
        ebs_iops: (4_000, 4_000),
        ebs_attachments: (39, "shared"),
        instance_store: Some((2, 420)),
        memory_mib: 7_680,
        vcpus: (2, 2, 1),
        network_performance: "Moderate",
    },
    Published {
        instance_type: "m1.xlarge",
        ebs_bandwidth: Some((1_000, 125.0, 1_000)),
        ebs_iops: (8_000, 8_000),
        ebs_attachments: (39, "shared"),
        instance_store: Some((4, 420)),
        memory_mib: 15_360,
        vcpus: (4, 4, 1),
        network_performance: "High",
    },
    Published {
        instance_type: "m2.xlarge",
        ebs_bandwidth: None,
        ebs_iops: (0, 0),
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 420)),
        memory_mib: 17_510,
        vcpus: (2, 2, 1),
        network_performance: "Moderate",
    },
    Published {
        instance_type: "m2.2xlarge",
        ebs_bandwidth: Some((500, 62.5, 500)),
        ebs_iops: (4_000, 4_000),
        ebs_attachments: (39, "shared"),
        instance_store: Some((1, 850)),
        memory_mib: 35_020,
        vcpus: (4, 4, 1),
        network_performance: "Moderate",
    },
    Published {
        instance_type: "m2.4xlarge",
        ebs_bandwidth: Some((1_000, 125.0, 1_000)),
        ebs_iops: (8_000, 8_000),
        ebs_attachments: (39, "shared"),
        instance_store: Some((2, 840)),
        memory_mib: 70_041,
        vcpus: (8, 8, 1),
        network_performance: "High",
    },
    Published {
        instance_type: "u-6tb1.56xlarge",
        ebs_bandwidth: Some((38_000, 4_750.0, 38_000)),
        ebs_iops: (160_000, 160_000),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        memory_mib: 6_291_456,
        vcpus: (224, 224, 1),
        network_performance: "100 Gigabit",
    },
    Published {
        instance_type: "u-6tb1.112xlarge",
        ebs_bandwidth: Some((38_000, 4_750.0, 38_000)),
        ebs_iops: (160_000, 160_000),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        memory_mib: 6_291_456,
        vcpus: (448, 224, 2),
        network_performance: "100 Gigabit",
    },
];

/// Rule: the EBS, memory, vCPU, instance store and network figures a type reports are the
/// ones AWS publishes for THAT type.
#[tokio::test]
async fn published_figures_are_reported_for_every_kind_of_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in PUBLISHED {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let reported_memory = whole(case, &body, &["memoryInfo", "sizeInMiB"]);
        assert_eq!(
            reported_memory, row.memory_mib,
            "case {case:?}: expected memoryInfo.sizeInMiB {}, got {reported_memory}",
            row.memory_mib
        );

        let (vcpus, cores, threads) = row.vcpus;
        for (member, expected) in [
            ("defaultVCpus", vcpus),
            ("defaultCores", cores),
            ("defaultThreadsPerCore", threads),
        ] {
            let got = whole(case, &body, &["vCpuInfo", member]);
            assert_eq!(
                got, expected,
                "case {case:?}: expected vCpuInfo.{member} {expected}, got {got}"
            );
        }

        let performance = required(case, &body, &["networkInfo", "networkPerformance"]);
        assert_eq!(
            performance, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {performance:?}",
            row.network_performance
        );

        let (attachments, limit_kind) = row.ebs_attachments;
        let got = whole(case, &body, &["ebsInfo", "maximumEbsAttachments"]);
        assert_eq!(
            got, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {got}"
        );
        let got = required(case, &body, &["ebsInfo", "attachmentLimitType"]);
        assert_eq!(
            got, limit_kind,
            "case {case:?}: expected attachmentLimitType {limit_kind:?}, got {got:?}"
        );

        match row.ebs_bandwidth {
            Some((baseline_mbps, baseline_mb_per_second, maximum_mbps)) => {
                let (baseline_iops, maximum_iops) = row.ebs_iops;
                for (member, expected) in [
                    ("baselineBandwidthInMbps", baseline_mbps),
                    ("baselineIops", baseline_iops),
                    ("maximumBandwidthInMbps", maximum_mbps),
                    ("maximumIops", maximum_iops),
                ] {
                    let got = whole(case, &body, &["ebsInfo", "ebsOptimizedInfo", member]);
                    assert_eq!(
                        got, expected,
                        "case {case:?}: expected {member} {expected}, got {got}"
                    );
                }
                // The throughput members are the bandwidth ones in the other unit, which is
                // how the published table's Mbps and MB/s columns relate — and a size whose
                // sustained figure is not a whole number of Mbps carries the UNtruncated
                // figure here, which is why it is a row of its own rather than the integer
                // above divided.
                for (member, expected) in [
                    ("baselineThroughputInMBps", baseline_mb_per_second),
                    (
                        "maximumThroughputInMBps",
                        maximum_mbps as f64 / MEGABITS_PER_MEGABYTE,
                    ),
                ] {
                    let got = number(case, &body, &["ebsInfo", "ebsOptimizedInfo", member]);
                    assert!(
                        (got - expected).abs() < THROUGHPUT_TOLERANCE,
                        "case {case:?}: expected {member} {expected}, got {got}"
                    );
                }
            }
            None => {
                let support = required(case, &body, &["ebsInfo", "ebsOptimizedSupport"]);
                assert_eq!(
                    support, "unsupported",
                    "case {case:?}: a type with no EBS-optimized performance reports no such \
                     mode, got {support:?}"
                );
                let performance = reported(case, &body, &["ebsInfo", "ebsOptimizedInfo"]);
                assert!(
                    performance.is_none(),
                    "case {case:?}: a type with no EBS-optimized mode describes no \
                     performance, got {performance:?}"
                );
            }
        }

        match row.instance_store {
            Some((devices, gb_each)) => {
                let got = whole(case, &body, &["instanceStorageInfo", "disks", "item", "count"]);
                assert_eq!(
                    got, devices,
                    "case {case:?}: expected {devices} local devices, got {got}"
                );
                let got =
                    whole(case, &body, &["instanceStorageInfo", "disks", "item", "sizeInGB"]);
                assert_eq!(
                    got, gb_each,
                    "case {case:?}: expected local devices of {gb_each} GB, got {got}"
                );
                let got = whole(case, &body, &["instanceStorageInfo", "totalSizeInGB"]);
                assert_eq!(
                    got,
                    devices * gb_each,
                    "case {case:?}: expected totalSizeInGB {}, got {got}",
                    devices * gb_each
                );
            }
            None => {
                let store = reported(case, &body, &["instanceStorageInfo", "totalSizeInGB"]);
                assert!(
                    store.is_none(),
                    "case {case:?}: a type with no local devices reports no \
                     instanceStorageInfo, got {store:?}"
                );
            }
        }
    }
}

/// Rule, the other half: a member AWS reports for every instance type is reported whatever
/// the answer is — "none", "unsupported" and zero are answers, not absences.
#[tokio::test]
async fn members_reported_for_every_type_carry_their_answer() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: one flex size, one previous generation size of each family, and both High Memory
    // sizes — each member being a flag or a count a client branches on.
    let rows = [
        ("c7i-flex.xlarge", vec!["cpuBurstModel"], "none"),
        ("c7i-flex.xlarge", vec!["dedicatedHostsSupported"], "false"),
        ("c7i-flex.xlarge", vec!["nitroEnclavesSupport"], "unsupported"),
        ("c7i-flex.xlarge", vec!["phcSupport"], "supported"),
        ("c7i-flex.xlarge", vec!["rebootMigrationSupport"], "supported"),
        (
            "c7i-flex.xlarge",
            vec!["networkInfo", "enaSrdSupported"],
            "false",
        ),
        (
            "c7i-flex.xlarge",
            vec!["networkInfo", "flexibleEnaQueuesSupport"],
            "unsupported",
        ),
        (
            "c7i-flex.xlarge",
            vec!["networkInfo", "secondaryNetworkSupported"],
            "false",
        ),
        (
            "c7i-flex.xlarge",
            vec!["networkInfo", "ipv4AddressesPerSecondaryInterface"],
            "0",
        ),
        (
            "c7i-flex.xlarge",
            vec!["networkInfo", "trafficMirroringSupport"],
            "supported",
        ),
        // A type that DOES support IPv6 reports as many addresses per interface as it does
        // private IPv4 ones, so the zero below is the answer for a type without it rather
        // than the member's only value.
        (
            "c7i-flex.xlarge",
            vec!["networkInfo", "ipv6AddressesPerInterface"],
            "15",
        ),
        ("m1.small", vec!["cpuBurstModel"], "none"),
        ("m1.small", vec!["phcSupport"], "unsupported"),
        ("m1.small", vec!["rebootMigrationSupport"], "unsupported"),
        ("m1.small", vec!["ebsInfo", "encryptionSupport"], "supported"),
        (
            "m1.small",
            vec!["networkInfo", "trafficMirroringSupport"],
            "unsupported",
        ),
        ("m1.small", vec!["networkInfo", "ipv6Supported"], "false"),
        (
            "m1.small",
            vec!["networkInfo", "ipv6AddressesPerInterface"],
            "0",
        ),
        (
            "m1.small",
            vec!["networkInfo", "maximumNetworkInterfaces"],
            "2",
        ),
        (
            "m1.small",
            vec!["networkInfo", "ipv4AddressesPerInterface"],
            "4",
        ),
        (
            "m1.medium",
            vec!["networkInfo", "maximumNetworkInterfaces"],
            "2",
        ),
        ("m2.xlarge", vec!["ebsInfo", "encryptionSupport"], "supported"),
        (
            "m2.2xlarge",
            vec!["ebsInfo", "ebsOptimizedSupport"],
            "supported",
        ),
        (
            "m2.2xlarge",
            vec!["networkInfo", "ipv4AddressesPerInterface"],
            "30",
        ),
        (
            "m2.2xlarge",
            vec!["networkInfo", "ipv6AddressesPerInterface"],
            "0",
        ),
        ("u-6tb1.112xlarge", vec!["nitroTpmSupport"], "unsupported"),
        ("u-6tb1.112xlarge", vec!["nitroEnclavesSupport"], "unsupported"),
        ("u-6tb1.112xlarge", vec!["phcSupport"], "unsupported"),
        (
            "u-6tb1.112xlarge",
            vec!["rebootMigrationSupport"],
            "supported",
        ),
        (
            "u-6tb1.112xlarge",
            vec!["networkInfo", "maximumNetworkCards"],
            "1",
        ),
        (
            "u-6tb1.56xlarge",
            vec!["networkInfo", "defaultNetworkCardIndex"],
            "0",
        ),
        // Connection tracking's default idle timeouts are reported for every type, whatever
        // hardware it runs on.
        (
            "m1.large",
            vec![
                "networkInfo",
                "connectionTrackingConfiguration",
                "defaultTcpEstablishedTimeout",
            ],
            "432000",
        ),
    ];

    for (instance_type, path, expected) in rows {
        let case = format!("{instance_type} {}", path.join("/"));
        let body = described(&case, &router, instance_type).await;
        let got = required(&case, &body, &path);
        assert_eq!(
            got, expected,
            "case {case:?}: expected {} {expected:?}, got {got:?}",
            path.join("/")
        );
    }
}

/// Rule: the LISTS a type publishes — what it is offered and what can be configured on it —
/// are its own.
#[tokio::test]
async fn published_lists_are_the_types_own() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: the flex family beside the family it shares its hardware with, so the pair
    // differs in the placement group strategies and the usage classes rather than in the
    // size; the previous generation families, whose AMIs may be either virtualization type;
    // and the High Memory family, offered one usage class, one boot mode and one core count.
    let rows: [(&str, Vec<&str>, Vec<&str>); 12] = [
        (
            "c7i-flex.2xlarge",
            vec!["placementGroupInfo", "supportedStrategies"],
            vec!["partition", "spread"],
        ),
        (
            "c7i.2xlarge",
            vec!["placementGroupInfo", "supportedStrategies"],
            vec!["cluster", "partition", "spread"],
        ),
        (
            "c7i-flex.2xlarge",
            vec!["processorInfo", "supportedFeatures"],
            vec!["nested-virtualization"],
        ),
        (
            "c7i-flex.2xlarge",
            vec!["supportedUsageClasses"],
            vec!["on-demand", "spot"],
        ),
        (
            "c7i-flex.2xlarge",
            vec!["supportedBootModes"],
            vec!["legacy-bios", "uefi"],
        ),
        (
            "m1.small",
            vec!["supportedVirtualizationTypes"],
            vec!["hvm", "paravirtual"],
        ),
        (
            "m2.4xlarge",
            vec!["supportedVirtualizationTypes"],
            vec!["hvm", "paravirtual"],
        ),
        (
            "m1.large",
            vec!["vCpuInfo", "validThreadsPerCore"],
            vec!["1"],
        ),
        (
            "u-6tb1.112xlarge",
            vec!["supportedUsageClasses"],
            vec!["on-demand"],
        ),
        (
            "u-6tb1.112xlarge",
            vec!["supportedBootModes"],
            vec!["legacy-bios"],
        ),
        (
            "u-6tb1.112xlarge",
            vec!["vCpuInfo", "validCores"],
            vec!["224"],
        ),
        (
            "u-6tb1.56xlarge",
            vec!["vCpuInfo", "validThreadsPerCore"],
            vec!["1"],
        ),
    ];

    for (instance_type, path, expected) in rows {
        let case = format!("{instance_type} {}", path.join("/"));
        let body = described(&case, &router, instance_type).await;
        let got = list(&case, &body, &path);
        assert_eq!(
            got,
            owned(&expected),
            "case {case:?}: expected {} {expected:?}, got {got:?}",
            path.join("/")
        );
    }
}

/// Rule, stated as what a formula over the name cannot produce: two types of ONE size that
/// agree on nothing, two sizes of one family that share every hardware figure while their
/// vCPUs differ, and a flex size that sustains half of what its sibling of the same size does
/// while both reach the same ceiling.
#[tokio::test]
async fn the_published_figures_do_not_follow_the_size_in_the_name() {
    let (router, _engine) = super::ec2_query::emulator();

    let baseline = |case: &str, body: &str| -> i64 {
        whole(case, body, &["ebsInfo", "ebsOptimizedInfo", "baselineBandwidthInMbps"])
    };
    let maximum = |case: &str, body: &str| -> i64 {
        whole(case, body, &["ebsInfo", "ebsOptimizedInfo", "maximumBandwidthInMbps"])
    };

    // One size component, two families: the vCPUs and the memory differ, so neither can be
    // read off the `xlarge`.
    let left = described("m1.xlarge", &router, "m1.xlarge").await;
    let right = described("m2.xlarge", &router, "m2.xlarge").await;
    let left_vcpus = whole("m1.xlarge", &left, &["vCpuInfo", "defaultVCpus"]);
    let right_vcpus = whole("m2.xlarge", &right, &["vCpuInfo", "defaultVCpus"]);
    assert!(
        left_vcpus > right_vcpus,
        "two families of one size publish their own vCPU counts, got {left_vcpus} and \
         {right_vcpus}"
    );
    let left_memory = whole("m1.xlarge", &left, &["memoryInfo", "sizeInMiB"]);
    let right_memory = whole("m2.xlarge", &right, &["memoryInfo", "sizeInMiB"]);
    assert!(
        right_memory > left_memory,
        "two families of one size publish their own memory, got {left_memory} and \
         {right_memory}"
    );

    // One family, two sizes: the same server with its second thread per core on and off, so
    // every hardware figure is shared while the vCPUs double.
    let half = described("u-6tb1.56xlarge", &router, "u-6tb1.56xlarge").await;
    let whole_host = described("u-6tb1.112xlarge", &router, "u-6tb1.112xlarge").await;
    assert_eq!(
        baseline("u-6tb1.56xlarge", &half),
        baseline("u-6tb1.112xlarge", &whole_host),
        "both sizes of the High Memory family publish the server's own EBS figure"
    );
    assert_eq!(
        whole("u-6tb1.56xlarge", &half, &["memoryInfo", "sizeInMiB"]),
        whole("u-6tb1.112xlarge", &whole_host, &["memoryInfo", "sizeInMiB"]),
        "both sizes carry the server's whole memory"
    );
    let half_vcpus = whole("u-6tb1.56xlarge", &half, &["vCpuInfo", "defaultVCpus"]);
    let whole_vcpus = whole("u-6tb1.112xlarge", &whole_host, &["vCpuInfo", "defaultVCpus"]);
    assert_eq!(
        whole_vcpus,
        half_vcpus * 2,
        "the second thread per core doubles the vCPUs, got {half_vcpus} and {whole_vcpus}"
    );
    assert_eq!(
        whole("u-6tb1.56xlarge", &half, &["vCpuInfo", "defaultCores"]),
        whole("u-6tb1.112xlarge", &whole_host, &["vCpuInfo", "defaultCores"]),
        "and leaves the cores it runs on untouched"
    );

    // One size, one host, two allocations of it: the flex size sustains half of what its
    // sibling does and reaches the same ceiling.
    let flex = described("c7i-flex.4xlarge", &router, "c7i-flex.4xlarge").await;
    let fixed = described("c7i.4xlarge", &router, "c7i.4xlarge").await;
    let flex_baseline = baseline("c7i-flex.4xlarge", &flex);
    let fixed_baseline = baseline("c7i.4xlarge", &fixed);
    assert_eq!(
        flex_baseline * 2,
        fixed_baseline,
        "the flex size sustains half its sibling's bandwidth, got {flex_baseline} against \
         {fixed_baseline}"
    );
    assert_eq!(
        maximum("c7i-flex.4xlarge", &flex),
        maximum("c7i.4xlarge", &fixed),
        "while both reach the same ceiling"
    );
}
