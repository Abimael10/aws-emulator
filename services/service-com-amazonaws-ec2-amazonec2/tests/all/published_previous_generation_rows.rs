// SPDX-License-Identifier: CC0-1.0

//! What AWS publishes about the instance types of the PREVIOUS GENERATION families, driven over
//! the wire this service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST
//! answered with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the catalog's: every member `DescribeInstanceTypes` reports about an
//! instance type is a PUBLISHED CONSTANT of that type, served from the type's own row, not a
//! figure computed from the size multiplier in its name. A previous generation family is where
//! that bites hardest, because nothing about it scales: its memory is the figure its own
//! specification states rather than a whole number of GiB per vCPU; its local store is the
//! devices it shipped with, two of them at every size of some families and one of another size
//! at every size of others, so the DEVICE SIZE is what changes with the size while the count
//! does not; EBS optimization is an option of the launch offered on some sizes of a family and
//! on neither its smallest nor its whole-host size, each offered size publishing ONE fixed
//! bandwidth and IOPS figure because a pre-Nitro instance has no baseline it bursts above; its
//! EBS volumes share one attachment total with its interfaces and its devices; its network
//! performance is the WORDS of the published column rather than a figure in Gigabit; a cluster
//! placement group is offered on the families AWS names and on no others; the packets of a
//! session are copied by hardware a pre-Nitro host does not have, so Traffic Mirroring is
//! offered on none of them; its processor's sustained frequency is that processor's; and the
//! smallest size of one of these families is among the few types AWS publishes 32-bit AMI
//! support for.
//!
//! The table spans what the rule generalises over: five sizes of one compute optimized family —
//! the smallest, the three the EBS-optimized option is offered on and the one that holds the
//! whole host, whose cores sit on two processor sockets — and two sizes of two OTHER previous
//! generation families, a general purpose one and a storage optimized one neither this report
//! nor its issue names, whose device size, attachment total, EBS mode and frequency are their
//! own. The figures come from AWS's "Previous generation instances" page, each family's own
//! specification, the "Amazon EBS optimized instance types" table, "Instance volume limits" and
//! the pages naming the types offered a cluster placement group and 32-bit AMIs.

use super::ec2_query::{
    assert_published_ebs, described_instance_type, reported_instance_type_list,
    reported_instance_type_member,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The `EbsOptimizedSupport` of a size the option is not offered on, which therefore has no
/// EBS-optimized performance to report at all.
const EBS_OPTIMIZED_UNSUPPORTED: &str = "unsupported";

/// The `EbsOptimizedSupport` of a size the option is offered on, as an option of the launch.
const EBS_OPTIMIZED_ON_REQUEST: &str = "supported";

/// The `PlacementGroupStrategy` values a type offered every strategy publishes, in AWS's order.
const EVERY_PLACEMENT_STRATEGY: [&str; 3] = ["cluster", "partition", "spread"];

/// The published facts of one size of one previous generation family.
///
/// A column is `None` where the row makes no claim about it: the two sizes of the other
/// families are here for the members their own rows publish, and a member the catalog does not
/// hold a published figure of for them is not what this suite is about.
#[derive(Debug, Clone)]
struct PublishedRow {
    /// `c3.2xlarge` and the like.
    instance_type: &'static str,
    /// Its memory in MiB, where its specification states a figure of its own.
    memory_mib: Option<i64>,
    /// Its local instance store as (devices, GB each).
    instance_store: (i64, i64),
    /// Whether the EBS-optimized mode is offered on the size.
    ebs_optimized: &'static str,
    /// Its four published EBS figures, where the mode is offered on it.
    ebs: Option<(f64, f64, f64, f64)>,
    /// The EBS volumes it holds attached, and whether that total is shared.
    ebs_attachments: (i64, &'static str),
    /// Its network performance, as the published column writes it.
    network_performance: &'static str,
    /// Whether Traffic Mirroring is offered on it.
    traffic_mirroring: Option<&'static str>,
    /// The placement group strategies it is offered.
    placement_strategies: Option<&'static [&'static str]>,
    /// Its processor's sustained all-core frequency, in GHz.
    sustained_ghz: f64,
    /// The architectures its AMIs may be built for.
    architectures: Option<&'static [&'static str]>,
    /// Whether it is offered on a Dedicated Host, and whether CloudWatch action based recovery
    /// is supported.
    host_features: Option<(bool, bool)>,
    /// The core counts a caller may configure it with.
    valid_cores: Option<&'static [i32]>,
}

/// The core counts of the whole-host size, whose cores sit on two processor sockets and are
/// taken one per socket.
const TWO_SOCKET_CORES: [i32; 8] = [2, 4, 6, 8, 10, 12, 14, 16];

/// Rule: every member of a previous generation type is served from that type's published row.
#[tokio::test]
async fn published_previous_generation_rows_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    let rows = [
        PublishedRow {
            instance_type: "c3.large",
            memory_mib: Some(3_840),
            instance_store: (2, 16),
            ebs_optimized: EBS_OPTIMIZED_UNSUPPORTED,
            ebs: None,
            ebs_attachments: (39, "shared"),
            network_performance: "Moderate",
            traffic_mirroring: Some("unsupported"),
            placement_strategies: Some(&EVERY_PLACEMENT_STRATEGY),
            sustained_ghz: 2.8,
            // The one size of this family AWS publishes 32-bit AMI support for.
            architectures: Some(&["i386", "x86_64"]),
            host_features: Some((true, true)),
            valid_cores: Some(&[1]),
        },
        PublishedRow {
            instance_type: "c3.xlarge",
            memory_mib: Some(7_680),
            instance_store: (2, 40),
            ebs_optimized: EBS_OPTIMIZED_ON_REQUEST,
            ebs: Some((500.0, 4_000.0, 500.0, 4_000.0)),
            ebs_attachments: (39, "shared"),
            network_performance: "Moderate",
            traffic_mirroring: Some("unsupported"),
            placement_strategies: Some(&EVERY_PLACEMENT_STRATEGY),
            sustained_ghz: 2.8,
            architectures: Some(&["x86_64"]),
            host_features: Some((true, true)),
            valid_cores: Some(&[1, 2]),
        },
        PublishedRow {
            instance_type: "c3.2xlarge",
            memory_mib: Some(15_360),
            instance_store: (2, 80),
            ebs_optimized: EBS_OPTIMIZED_ON_REQUEST,
            ebs: Some((1_000.0, 8_000.0, 1_000.0, 8_000.0)),
            ebs_attachments: (39, "shared"),
            network_performance: "High",
            traffic_mirroring: Some("unsupported"),
            placement_strategies: Some(&EVERY_PLACEMENT_STRATEGY),
            sustained_ghz: 2.8,
            architectures: Some(&["x86_64"]),
            host_features: Some((true, true)),
            valid_cores: Some(&[1, 2, 3, 4]),
        },
        PublishedRow {
            instance_type: "c3.4xlarge",
            memory_mib: Some(30_720),
            instance_store: (2, 160),
            ebs_optimized: EBS_OPTIMIZED_ON_REQUEST,
            ebs: Some((2_000.0, 16_000.0, 2_000.0, 16_000.0)),
            ebs_attachments: (39, "shared"),
            network_performance: "High",
            traffic_mirroring: Some("unsupported"),
            placement_strategies: Some(&EVERY_PLACEMENT_STRATEGY),
            sustained_ghz: 2.8,
            architectures: Some(&["x86_64"]),
            host_features: Some((true, true)),
            valid_cores: Some(&[1, 2, 3, 4, 5, 6, 7, 8]),
        },
        PublishedRow {
            instance_type: "c3.8xlarge",
            memory_mib: Some(61_440),
            instance_store: (2, 320),
            ebs_optimized: EBS_OPTIMIZED_UNSUPPORTED,
            ebs: None,
            ebs_attachments: (39, "shared"),
            network_performance: "10 Gigabit",
            traffic_mirroring: Some("unsupported"),
            placement_strategies: Some(&EVERY_PLACEMENT_STRATEGY),
            sustained_ghz: 2.8,
            architectures: Some(&["x86_64"]),
            host_features: Some((true, true)),
            valid_cores: Some(&TWO_SOCKET_CORES),
        },
        // A general purpose previous generation family: one device of its own size per step of
        // the ladder, its own frequency, and the same shared attachment total.
        PublishedRow {
            instance_type: "m3.xlarge",
            memory_mib: Some(15_360),
            instance_store: (2, 40),
            ebs_optimized: EBS_OPTIMIZED_ON_REQUEST,
            ebs: Some((500.0, 4_000.0, 500.0, 4_000.0)),
            ebs_attachments: (39, "shared"),
            network_performance: "High",
            traffic_mirroring: None,
            placement_strategies: None,
            sustained_ghz: 2.5,
            architectures: Some(&["x86_64"]),
            host_features: Some((true, true)),
            valid_cores: None,
        },
        // A storage optimized one, whose devices ARE the family: 800 GB each at every size, so
        // the count grows while the device does not.
        PublishedRow {
            instance_type: "i2.2xlarge",
            memory_mib: None,
            instance_store: (2, 800),
            ebs_optimized: EBS_OPTIMIZED_ON_REQUEST,
            ebs: Some((1_000.0, 8_000.0, 1_000.0, 8_000.0)),
            ebs_attachments: (40, "shared"),
            network_performance: "High",
            traffic_mirroring: Some("unsupported"),
            placement_strategies: Some(&EVERY_PLACEMENT_STRATEGY),
            sustained_ghz: 2.5,
            architectures: None,
            host_features: Some((true, false)),
            valid_cores: None,
        },
    ];

    for row in rows {
        let case = row.instance_type;
        let body = described_instance_type(case, &router, REGION, row.instance_type).await;

        if let Some(memory_mib) = row.memory_mib {
            let reported = reported_instance_type_member(case, &body, &["memoryInfo", "sizeInMiB"]);
            assert_eq!(
                reported.as_deref(),
                Some(memory_mib.to_string().as_str()),
                "case {case:?}: expected {memory_mib} MiB of memory, got {reported:?}"
            );
        }

        let (devices, gb_each) = row.instance_store;
        for (path, expected) in [
            (["instanceStorageInfo", "disks", "item", "count"], devices),
            (
                ["instanceStorageInfo", "disks", "item", "sizeInGB"],
                gb_each,
            ),
        ] {
            let reported = reported_instance_type_member(case, &body, &path);
            assert_eq!(
                reported.as_deref(),
                Some(expected.to_string().as_str()),
                "case {case:?}: expected {} {expected}, got {reported:?}",
                path[path.len() - 1]
            );
        }
        let total = reported_instance_type_member(case, &body, &["instanceStorageInfo", "totalSizeInGB"]);
        assert_eq!(
            total.as_deref(),
            Some((devices * gb_each).to_string().as_str()),
            "case {case:?}: the store is {devices} devices of {gb_each} GB, got {total:?}"
        );

        let optimized =
            reported_instance_type_member(case, &body, &["ebsInfo", "ebsOptimizedSupport"]);
        assert_eq!(
            optimized.as_deref(),
            Some(row.ebs_optimized),
            "case {case:?}: expected ebsOptimizedSupport {:?}, got {optimized:?}",
            row.ebs_optimized
        );
        match row.ebs {
            Some(ebs) => assert_published_ebs(case, &body, ebs),
            // A size the mode is not offered on has no performance to describe, so the
            // structure is absent rather than reported with invented figures.
            None => {
                let figure = reported_instance_type_member(
                    case,
                    &body,
                    &["ebsInfo", "ebsOptimizedInfo", "baselineIops"],
                );
                assert_eq!(
                    figure, None,
                    "case {case:?}: a size with no EBS-optimized mode describes no \
                     performance, got {figure:?}"
                );
            }
        }
        let (attachments, limit_kind) = row.ebs_attachments;
        for (member, expected) in [
            ("maximumEbsAttachments", attachments.to_string()),
            ("attachmentLimitType", limit_kind.to_string()),
        ] {
            let reported = reported_instance_type_member(case, &body, &["ebsInfo", member]);
            assert_eq!(
                reported.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {member} {expected:?}, got {reported:?}"
            );
        }

        let performance =
            reported_instance_type_member(case, &body, &["networkInfo", "networkPerformance"]);
        assert_eq!(
            performance.as_deref(),
            Some(row.network_performance),
            "case {case:?}: expected the published network performance {:?}, got \
             {performance:?}",
            row.network_performance
        );
        if let Some(mirroring) = row.traffic_mirroring {
            let reported = reported_instance_type_member(
                case,
                &body,
                &["networkInfo", "trafficMirroringSupport"],
            );
            assert_eq!(
                reported.as_deref(),
                Some(mirroring),
                "case {case:?}: expected trafficMirroringSupport {mirroring:?}, got \
                 {reported:?}"
            );
        }

        if let Some(strategies) = row.placement_strategies {
            let reported =
                reported_instance_type_list(case, &body, &["placementGroupInfo", "supportedStrategies"]);
            assert_eq!(
                reported, strategies,
                "case {case:?}: expected the placement strategies {strategies:?}, got \
                 {reported:?}"
            );
        }

        let clock =
            reported_instance_type_member(case, &body, &["processorInfo", "sustainedClockSpeedInGhz"]);
        let clock: f64 = clock
            .as_deref()
            .unwrap_or_else(|| panic!("case {case:?}: no sustained clock speed in {body}"))
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: the clock speed must be a number"));
        assert!(
            (clock - row.sustained_ghz).abs() < 0.001,
            "case {case:?}: expected a sustained {} GHz, got {clock}",
            row.sustained_ghz
        );
        if let Some(architectures) = row.architectures {
            let reported = reported_instance_type_list(
                case,
                &body,
                &["processorInfo", "supportedArchitectures"],
            );
            assert_eq!(
                reported, architectures,
                "case {case:?}: expected the architectures {architectures:?}, got {reported:?}"
            );
        }

        if let Some((dedicated_hosts, auto_recovery)) = row.host_features {
            for (member, expected) in [
                ("dedicatedHostsSupported", dedicated_hosts),
                ("autoRecoverySupported", auto_recovery),
            ] {
                let reported = reported_instance_type_member(case, &body, &[member]);
                assert_eq!(
                    reported.as_deref(),
                    Some(expected.to_string().as_str()),
                    "case {case:?}: expected {member} {expected}, got {reported:?}"
                );
            }
        }

        if let Some(cores) = row.valid_cores {
            let reported = reported_instance_type_list(case, &body, &["vCpuInfo", "validCores"]);
            let expected: Vec<String> = cores.iter().map(i32::to_string).collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the core counts {expected:?}, got {reported:?}"
            );
        }
    }
}
