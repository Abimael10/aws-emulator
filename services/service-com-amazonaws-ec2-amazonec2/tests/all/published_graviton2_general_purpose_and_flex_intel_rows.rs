// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for the sixth generation AWS Graviton
//! general purpose family and for the eighth generation Intel FLEXIBLE general purpose one —
//! driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: EVERY MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE TYPE IS
//! THE FIGURE AWS PUBLISHES FOR THAT ONE TYPE, not a function of the size multiplier in its
//! name. The four EBS figures (`EbsInfo$EbsOptimizedInfo`, whose baseline and maximum are
//! independent members), how many volumes the type holds attached and whether that limit is
//! its own (`$MaximumEbsAttachments` / `$AttachmentLimitType`), its network performance, its
//! interface and address limits, the weightings its bandwidth may be split with, its
//! processor's sustained frequency and the host features it is offered — ENA Express, Nitro
//! Enclaves, Traffic Mirroring, a Dedicated Host, a placement group strategy, an interface
//! attachment from a secondary network — are each a published fact of the type.
//!
//! The tables below span what the rule generalises over: two families differing in processor
//! (AWS Graviton and Intel), in generation (a 2019 family and a 2025 one), in whether their
//! share of the host is a fixed allocation at all (the `flex` family's is not) and in whether
//! their EBS limit is shared with their other devices or the volumes' own; inside each family
//! every step of the ladder is a row, so no one size is the specification — the Graviton
//! family's sustained EBS bandwidth is above a share of its largest size's figure at the three
//! smallest sizes and its `12xlarge`'s IOPS stops below the share its bandwidth would give it,
//! while the flexible family publishes one flat figure per `xlarge` of width and bursts to the
//! ceiling of its own row.
//!
//! The figures are the ones AWS states for each type: its family's specification page (network
//! performance, the processor's sustained frequency), the "Amazon EBS optimized instance
//! types" table (the EBS figures), "Instance volume limits" (the attachment limit and its
//! kind), the "IP addresses per network interface per instance type" table and the published
//! lists of the types offered ENA Express, Nitro Enclaves, Traffic Mirroring, Dedicated Hosts
//! and each placement group strategy.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    assert_published_ebs, described_instance_type, emulator,
    instance_type_member_number as number, reported_instance_type_list,
    reported_instance_type_member, required_instance_type_member as required,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// The published row of one instance type.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `m6g.medium` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (f64, f64, f64, f64),
    /// The EBS volumes it holds attached, and whether that limit is shared with its
    /// interfaces and local devices or is the volumes' own.
    ebs_attachments: (i64, &'static str),
    /// Its network performance, as the published table writes that column.
    network_performance: &'static str,
    /// The interfaces it can have attached, and the private IPv4 addresses each takes — which
    /// is also how many IPv6 addresses each takes on a type that supports IPv6.
    network_limits: (i64, i64),
    /// Its processor's sustained frequency, in GHz.
    sustained_ghz: f64,
    /// Whether ENA Express is offered on it (`NetworkInfo$EnaSrdSupported`).
    ena_srd: &'static str,
    /// Whether Nitro Enclaves is (`InstanceTypeInfo$NitroEnclavesSupport`).
    nitro_enclaves: &'static str,
    /// Whether a session's packets can be copied under its guest
    /// (`networkInfo`'s `trafficMirroringSupport`).
    traffic_mirroring: &'static str,
    /// Whether it is offered on a Dedicated Host.
    dedicated_hosts: &'static str,
    /// Whether CloudWatch action based recovery is offered
    /// (`InstanceTypeInfo$AutoRecoverySupported`), whether an instance is moved to another host
    /// on a user-initiated reboot (`$RebootMigrationSupport`) and whether the type carries a
    /// local PTP hardware clock (`$PhcSupport`) — the three a bare metal size answers on its
    /// HOST's behalf rather than on a hypervisor's.
    host_features: (&'static str, &'static str, &'static str),
}

/// Every step of both families' ladders, with the figures AWS publishes for each.
///
/// M6g: general purpose on AWS Graviton2, carved out of the same 64-vCPU host as its compute
/// optimized sibling — a baseline that bursts to 4,750 Mbps and 20,000 IOPS below the
/// `4xlarge` and a dedicated figure from it, a network ladder that reaches the host's 25
/// Gigabit, the two interfaces of four addresses its smallest size takes, and the 31 volumes
/// its bare metal size holds out of the host's shared pool.
/// M8i-flex: general purpose on Intel Xeon 6 with a FLEXIBLE share of the host — 625 Mbps of
/// EBS per `xlarge` of width from the `2xlarge` up with its own two smallest rows below that,
/// a limit of the volumes' own, interface and address limits well above the earlier ladder's,
/// a network figure that is a burst ceiling at every size, and neither ENA Express nor Nitro
/// Enclaves nor a Dedicated Host nor Traffic Mirroring.
const ROWS: [Row; 16] = [
    Row {
        instance_type: "m6g.medium",
        ebs: (315.0, 2_500.0, 4_750.0, 20_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "Up to 10 Gigabit",
        network_limits: (2, 4),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.large",
        ebs: (630.0, 3_600.0, 4_750.0, 20_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "Up to 10 Gigabit",
        network_limits: (3, 10),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "supported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.xlarge",
        ebs: (1_188.0, 6_000.0, 4_750.0, 20_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "Up to 10 Gigabit",
        network_limits: (4, 15),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "supported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.2xlarge",
        ebs: (2_375.0, 12_000.0, 4_750.0, 20_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "Up to 10 Gigabit",
        network_limits: (4, 15),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "supported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.4xlarge",
        ebs: (4_750.0, 20_000.0, 4_750.0, 20_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "Up to 10 Gigabit",
        network_limits: (8, 30),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "supported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.8xlarge",
        ebs: (9_500.0, 40_000.0, 9_500.0, 40_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "12 Gigabit",
        network_limits: (8, 30),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "supported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.12xlarge",
        ebs: (14_250.0, 50_000.0, 14_250.0, 50_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "20 Gigabit",
        network_limits: (8, 30),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "supported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.16xlarge",
        ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
        ebs_attachments: (27, "shared"),
        network_performance: "25 Gigabit",
        network_limits: (15, 50),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "supported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m6g.metal",
        ebs: (19_000.0, 80_000.0, 19_000.0, 80_000.0),
        ebs_attachments: (31, "shared"),
        network_performance: "25 Gigabit",
        network_limits: (15, 50),
        sustained_ghz: 2.5,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "supported",
        dedicated_hosts: "true",
        host_features: ("true", "supported", "unsupported"),
    },
    Row {
        instance_type: "m8i-flex.large",
        ebs: (315.0, 2_500.0, 10_000.0, 40_000.0),
        ebs_attachments: (32, "dedicated"),
        network_performance: "Up to 12.5 Gigabit",
        network_limits: (3, 20),
        sustained_ghz: 3.9,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "unsupported",
        dedicated_hosts: "false",
        host_features: ("true", "supported", "supported"),
    },
    Row {
        instance_type: "m8i-flex.xlarge",
        ebs: (630.0, 3_600.0, 10_000.0, 40_000.0),
        ebs_attachments: (32, "dedicated"),
        network_performance: "Up to 12.5 Gigabit",
        network_limits: (4, 30),
        sustained_ghz: 3.9,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "unsupported",
        dedicated_hosts: "false",
        host_features: ("true", "supported", "supported"),
    },
    Row {
        instance_type: "m8i-flex.2xlarge",
        ebs: (1_250.0, 6_000.0, 10_000.0, 40_000.0),
        ebs_attachments: (32, "dedicated"),
        network_performance: "Up to 15 Gigabit",
        network_limits: (4, 30),
        sustained_ghz: 3.9,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "unsupported",
        dedicated_hosts: "false",
        host_features: ("true", "supported", "supported"),
    },
    Row {
        instance_type: "m8i-flex.4xlarge",
        ebs: (2_500.0, 12_000.0, 10_000.0, 40_000.0),
        ebs_attachments: (32, "dedicated"),
        network_performance: "Up to 15 Gigabit",
        network_limits: (8, 50),
        sustained_ghz: 3.9,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "unsupported",
        dedicated_hosts: "false",
        host_features: ("true", "supported", "supported"),
    },
    Row {
        instance_type: "m8i-flex.8xlarge",
        ebs: (5_000.0, 20_000.0, 10_000.0, 40_000.0),
        ebs_attachments: (32, "dedicated"),
        network_performance: "Up to 15 Gigabit",
        network_limits: (10, 50),
        sustained_ghz: 3.9,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "unsupported",
        dedicated_hosts: "false",
        host_features: ("true", "supported", "supported"),
    },
    Row {
        instance_type: "m8i-flex.12xlarge",
        ebs: (7_500.0, 30_000.0, 15_000.0, 60_000.0),
        ebs_attachments: (32, "dedicated"),
        network_performance: "Up to 22.5 Gigabit",
        network_limits: (12, 50),
        sustained_ghz: 3.9,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "unsupported",
        dedicated_hosts: "false",
        host_features: ("true", "supported", "supported"),
    },
    Row {
        instance_type: "m8i-flex.16xlarge",
        ebs: (10_000.0, 40_000.0, 20_000.0, 80_000.0),
        ebs_attachments: (48, "dedicated"),
        network_performance: "Up to 30 Gigabit",
        network_limits: (16, 64),
        sustained_ghz: 3.9,
        ena_srd: "false",
        nitro_enclaves: "unsupported",
        traffic_mirroring: "unsupported",
        dedicated_hosts: "false",
        host_features: ("true", "supported", "supported"),
    },
];

/// Rule: every member a type reports is the figure AWS publishes for THAT type.
#[tokio::test]
async fn published_rows_report_the_facts_of_the_type() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        assert_published_ebs(case, &body, row.ebs);

        let (attachments, limit_type) = row.ebs_attachments;
        let reported = number(case, &body, &["ebsInfo", "maximumEbsAttachments"]) as i64;
        assert_eq!(
            reported, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {reported}"
        );
        let reported = required(case, &body, &["ebsInfo", "attachmentLimitType"]);
        assert_eq!(
            reported, limit_type,
            "case {case:?}: expected attachmentLimitType {limit_type:?}, got {reported:?}"
        );

        let reported = required(case, &body, &["networkInfo", "networkPerformance"]);
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );

        let (interfaces, addresses) = row.network_limits;
        let reported = number(case, &body, &["networkInfo", "maximumNetworkInterfaces"]) as i64;
        assert_eq!(
            reported, interfaces,
            "case {case:?}: expected maximumNetworkInterfaces {interfaces}, got {reported}"
        );
        // A type that supports IPv6 takes as many IPv6 addresses per interface as it takes
        // private IPv4 ones, so one published figure answers both members.
        for member in ["ipv4AddressesPerInterface", "ipv6AddressesPerInterface"] {
            let reported = number(case, &body, &["networkInfo", member]) as i64;
            assert_eq!(
                reported, addresses,
                "case {case:?}: expected {member} {addresses}, got {reported}"
            );
        }

        let reported = number(case, &body, &["processorInfo", "sustainedClockSpeedInGhz"]);
        assert!(
            (reported - row.sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {}, got {reported}",
            row.sustained_ghz
        );

        for (member, expected) in [
            (vec!["networkInfo", "enaSrdSupported"], row.ena_srd),
            (vec!["nitroEnclavesSupport"], row.nitro_enclaves),
            (
                vec!["networkInfo", "trafficMirroringSupport"],
                row.traffic_mirroring,
            ),
            (vec!["dedicatedHostsSupported"], row.dedicated_hosts),
            (vec!["autoRecoverySupported"], row.host_features.0),
            (vec!["rebootMigrationSupport"], row.host_features.1),
            (vec!["phcSupport"], row.host_features.2),
        ] {
            let reported = required(case, &body, &member);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {member:?} {expected:?}, got {reported:?}"
            );
        }
    }
}

/// Rule, applied to what separates a FLEXIBLE share of a host from a fixed one: the flexible
/// family publishes the weightings its bandwidth may be split with, the placement group
/// strategies that place an instance whose share is not a fixed allocation, its processor's
/// additional feature and this hardware generation's own established-TCP idle timeout, and it
/// takes no interface attachment from a secondary network — while the Graviton family, whose
/// share IS fixed, publishes no weighting at all, every strategy and the earlier timeout.
#[tokio::test]
async fn the_flexible_family_publishes_what_its_share_of_the_host_allows() {
    let (router, _engine) = emulator();

    for instance_type in ["m8i-flex.large", "m8i-flex.8xlarge", "m8i-flex.16xlarge"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;

        let reported =
            reported_instance_type_list(case, &body, &["networkInfo", "bandwidthWeightings"]);
        assert_eq!(
            reported,
            vec!["default", "ebs-1", "vpc-1"],
            "case {case:?}: expected the configurable bandwidth weightings, got {reported:?}"
        );
        let reported = reported_instance_type_list(
            case,
            &body,
            &["placementGroupInfo", "supportedStrategies"],
        );
        assert_eq!(
            reported,
            vec!["partition", "spread"],
            "case {case:?}: a type whose share of its host is not a fixed allocation is \
             offered no cluster placement group, got {reported:?}"
        );
        let reported =
            reported_instance_type_list(case, &body, &["processorInfo", "supportedFeatures"]);
        assert_eq!(
            reported,
            vec!["nested-virtualization"],
            "case {case:?}: expected the processor feature the family publishes, got {reported:?}"
        );
        let reported = number(
            case,
            &body,
            &[
                "networkInfo",
                "connectionTrackingConfiguration",
                "defaultTcpEstablishedTimeout",
            ],
        ) as i64;
        assert_eq!(
            reported, 350,
            "case {case:?}: expected this hardware generation's established-TCP idle timeout, \
             got {reported}"
        );
        let reported = required(case, &body, &["networkInfo", "secondaryNetworkSupported"]);
        assert_eq!(
            reported, "false",
            "case {case:?}: expected secondaryNetworkSupported false, got {reported:?}"
        );
        let reported = number(
            case,
            &body,
            &["networkInfo", "ipv4AddressesPerSecondaryInterface"],
        ) as i64;
        assert_eq!(
            reported, 0,
            "case {case:?}: a type that takes no secondary attachment has no private address \
             available on one, got {reported}"
        );
    }

    for instance_type in ["m6g.medium", "m6g.12xlarge", "m6g.metal"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;

        let reported =
            reported_instance_type_member(case, &body, &["networkInfo", "bandwidthWeightings"]);
        assert!(
            reported.is_none(),
            "case {case:?}: a type whose split between network and EBS bandwidth is fixed \
             publishes no weighting, got {reported:?}"
        );
        let reported = reported_instance_type_list(
            case,
            &body,
            &["placementGroupInfo", "supportedStrategies"],
        );
        assert_eq!(
            reported,
            vec!["cluster", "partition", "spread"],
            "case {case:?}: expected every placement group strategy, got {reported:?}"
        );
        let reported = number(
            case,
            &body,
            &[
                "networkInfo",
                "connectionTrackingConfiguration",
                "defaultTcpEstablishedTimeout",
            ],
        ) as i64;
        assert_eq!(
            reported, 432_000,
            "case {case:?}: expected the established-TCP idle timeout of the earlier hardware, \
             got {reported}"
        );
    }
}

/// Rule, as an invariant over every row: a type sustains no more than it reaches, and the
/// published figures do not follow the size multiplier — the two families disagree at every
/// shared size, and the Graviton family's `4xlarge` sustains what the flexible family's
/// `16xlarge` reports as a ceiling.
#[tokio::test]
async fn the_published_figures_are_bounded_and_do_not_follow_the_size_multiplier() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        // Read BOTH figures off the response, so the invariant is checked on what the service
        // serves rather than on the table's own constants.
        let figure = |member: &str| {
            number(case, &body, &["ebsInfo", "ebsOptimizedInfo", member])
        };
        let (baseline_mbps, baseline_iops) = (figure("baselineBandwidthInMbps"), figure("baselineIops"));
        let (maximum_mbps, maximum_iops) = (figure("maximumBandwidthInMbps"), figure("maximumIops"));
        assert!(
            baseline_mbps <= maximum_mbps && baseline_iops <= maximum_iops,
            "case {case:?}: a type sustains no more than it reaches, got baseline \
             {baseline_mbps} Mbps / {baseline_iops} IOPS against maximum {maximum_mbps} Mbps / \
             {maximum_iops} IOPS"
        );
        let (_, _, published_maximum_mbps, _) = row.ebs;
        assert!(
            (maximum_mbps - published_maximum_mbps).abs() < 0.001,
            "case {case:?}: expected maximumBandwidthInMbps {published_maximum_mbps}, got \
             {maximum_mbps}"
        );
    }

    for (graviton, flexible) in [
        ("m6g.large", "m8i-flex.large"),
        ("m6g.2xlarge", "m8i-flex.2xlarge"),
        ("m6g.16xlarge", "m8i-flex.16xlarge"),
    ] {
        let case = format!("{graviton} against {flexible}");
        let left = described(&case, &router, graviton).await;
        let right = described(&case, &router, flexible).await;
        let path = ["ebsInfo", "ebsOptimizedInfo", "baselineBandwidthInMbps"];
        let sustained = number(&case, &left, &path);
        let other = number(&case, &right, &path);
        assert!(
            (sustained - other).abs() > 0.001,
            "case {case:?}: two families of one size publish their own figures, got \
             {sustained} Mbps for both"
        );
    }
}
