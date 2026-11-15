// SPDX-License-Identifier: CC0-1.0

//! The published catalog rows of an AMD x86 family and of two AWS Graviton families, driven
//! over the wire this service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST
//! answered with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: every member `DescribeInstanceTypes` reports that AWS publishes PER
//! INSTANCE TYPE is served from that type's own row of the catalog, never computed from the
//! size multiplier in its name. That covers the EBS performance, attachment limit and limit
//! kind (`EbsInfo`), the network performance, interface and address limits, bandwidth
//! weightings, Elastic Fabric Adapter and ENA Express support (`NetworkInfo`), the processor's
//! sustained frequency (`ProcessorInfo`), the boot modes, and whether hibernation, CloudWatch
//! action based recovery, reboot migration, a local PTP hardware clock, Nitro Enclaves and
//! Traffic Mirroring are offered on the type.
//!
//! The table spans what the rule generalises over rather than one page of the catalog: three
//! families on three different processors (AMD EPYC with two threads per core, AWS Graviton2,
//! and AWS Graviton4 whose bandwidth weightings are configurable), and of each the smallest
//! size, a size whose bandwidth still bursts, the largest virtualized size and a bare metal
//! size — so two sizes of one family agree on nothing a size multiplier would give, and two
//! families at one size agree on nothing either. Every expected figure is the one the family's
//! own specification page, the "Amazon EBS optimized instance types" table, "Instance volume
//! limits" and the "IP addresses per network interface per instance type" table publish for
//! that ONE type; the members are the ones
//! `smithy_query(query="shape",shape="InstanceTypeInfo,NetworkInfo,EbsInfo,EbsOptimizedInfo")`
//! declares, plus the `cpuBurstModel` and `trafficMirroringSupport` elements the live wire
//! renders on those shapes.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, indexed, owned};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The megabits in a megabyte, which is all that separates the `Mbps` figures of
/// `EbsOptimizedInfo` from its `MB/s` ones.
const MEGABITS_PER_MEGABYTE: f64 = 8.0;

/// How close two reported figures have to be to count as the same one: both are written with
/// at most three decimals, so anything below that is the same value.
const TOLERANCE: f64 = 0.001;

/// The bandwidth weightings a type whose network and EBS split is configurable publishes
/// (`NetworkInfo$BandwidthWeightings`); a type whose split is fixed publishes none at all.
const CONFIGURABLE_WEIGHTINGS: &[&str] = &["default", "ebs-1", "vpc-1"];

/// `cpuBurstModel` of a type whose vCPUs run at their full rate rather than on CPU credits.
const NO_BURST_MODEL: &str = "none";

/// The published facts of ONE instance type: one row of the table under test.
#[derive(Debug, Clone, Copy)]
struct CatalogRow {
    /// `c6a.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// How many Amazon EBS volumes it can hold attached, and whether that limit is the EBS
    /// devices' own (`dedicated`) or shared with its interfaces and instance store (`shared`).
    attachments: (i64, &'static str),
    /// "The network performance", as the published table writes it.
    network_performance: &'static str,
    /// (most network interfaces, most private IPv4 addresses on each).
    interfaces: (i64, i64),
    /// The bandwidth weightings it publishes, empty where its split is fixed.
    bandwidth_weightings: &'static [&'static str],
    /// Is it offered an Elastic Fabric Adapter, and ENA Express?
    efa_and_ena_srd: (bool, bool),
    /// Can it hibernate, and is CloudWatch action based recovery offered on it?
    hibernation_and_recovery: (bool, bool),
    /// (`rebootMigrationSupport`, `phcSupport`, `nitroEnclavesSupport`,
    /// `trafficMirroringSupport`), each the enum member the type reports.
    host_features: (&'static str, &'static str, &'static str, &'static str),
    /// The boot modes it is offered with.
    boot_modes: &'static [&'static str],
    /// Its processor's sustained all-core frequency, in GHz.
    sustained_ghz: f64,
}

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(router: &MultiServiceRouter, instance_type: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeInstanceTypes",
        &indexed("InstanceType", &[instance_type]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {instance_type:?}: expected HTTP 200, got {status} with body {body}"
    );
    body
}

/// One reported member, or a panic naming the case when it is absent.
fn required(case: &str, xml: &str, name: &str) -> String {
    element(xml, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> reported, got {xml}"))
}

/// One reported member as a number.
fn number(case: &str, xml: &str, name: &str) -> f64 {
    let text = required(case, xml, name);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: <{name}> must be a number, got {text:?}"))
}

/// The entries of a reported list member.
fn list(case: &str, xml: &str, name: &str) -> Vec<String> {
    elements(&required(case, xml, name), "item")
}

/// The rows under test: four sizes of an AMD x86 compute optimized family, four of a Graviton2
/// one and four of a Graviton4 memory optimized one, including sizes no report of this defect
/// named (`c6a.2xlarge`, `c6g.xlarge`, `r8g.16xlarge`).
const ROWS: &[CatalogRow] = &[
    CatalogRow {
        instance_type: "c6a.2xlarge",
        ebs: (2_500, 12_000, 10_000, 40_000),
        attachments: (27, "shared"),
        network_performance: "Up to 12.5 Gigabit",
        interfaces: (4, 15),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (false, false),
        hibernation_and_recovery: (true, true),
        host_features: ("supported", "unsupported", "supported", "supported"),
        boot_modes: &["legacy-bios", "uefi"],
        sustained_ghz: 3.6,
    },
    CatalogRow {
        instance_type: "c6a.12xlarge",
        ebs: (15_000, 60_000, 15_000, 60_000),
        attachments: (27, "shared"),
        network_performance: "18.75 Gigabit",
        interfaces: (8, 30),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (false, true),
        hibernation_and_recovery: (false, true),
        host_features: ("supported", "unsupported", "supported", "supported"),
        boot_modes: &["legacy-bios", "uefi"],
        sustained_ghz: 3.6,
    },
    CatalogRow {
        instance_type: "c6a.48xlarge",
        ebs: (40_000, 240_000, 40_000, 240_000),
        attachments: (27, "shared"),
        network_performance: "50 Gigabit",
        interfaces: (15, 50),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (true, true),
        hibernation_and_recovery: (false, true),
        host_features: ("supported", "unsupported", "supported", "supported"),
        boot_modes: &["legacy-bios", "uefi"],
        sustained_ghz: 3.6,
    },
    CatalogRow {
        instance_type: "c6a.metal",
        ebs: (40_000, 240_000, 40_000, 240_000),
        attachments: (31, "shared"),
        network_performance: "50 Gigabit",
        interfaces: (15, 50),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (true, true),
        hibernation_and_recovery: (false, true),
        host_features: ("supported", "unsupported", "unsupported", "unsupported"),
        boot_modes: &["legacy-bios"],
        sustained_ghz: 3.6,
    },
    CatalogRow {
        instance_type: "c6g.medium",
        ebs: (315, 2_500, 4_750, 20_000),
        attachments: (27, "shared"),
        network_performance: "Up to 10 Gigabit",
        interfaces: (2, 4),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (false, false),
        hibernation_and_recovery: (true, true),
        host_features: ("supported", "unsupported", "unsupported", "supported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.5,
    },
    CatalogRow {
        instance_type: "c6g.xlarge",
        ebs: (1_188, 6_000, 4_750, 20_000),
        attachments: (27, "shared"),
        network_performance: "Up to 10 Gigabit",
        interfaces: (4, 15),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (false, false),
        hibernation_and_recovery: (true, true),
        host_features: ("supported", "unsupported", "supported", "supported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.5,
    },
    CatalogRow {
        instance_type: "c6g.16xlarge",
        ebs: (19_000, 80_000, 19_000, 80_000),
        attachments: (27, "shared"),
        network_performance: "25 Gigabit",
        interfaces: (15, 50),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (false, false),
        hibernation_and_recovery: (true, true),
        host_features: ("supported", "unsupported", "supported", "supported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.5,
    },
    CatalogRow {
        instance_type: "c6g.metal",
        ebs: (19_000, 80_000, 19_000, 80_000),
        attachments: (31, "shared"),
        network_performance: "25 Gigabit",
        interfaces: (15, 50),
        bandwidth_weightings: &[],
        efa_and_ena_srd: (false, false),
        hibernation_and_recovery: (false, true),
        host_features: ("supported", "unsupported", "unsupported", "supported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.5,
    },
    CatalogRow {
        instance_type: "r8g.medium",
        ebs: (315, 2_500, 10_000, 40_000),
        attachments: (32, "dedicated"),
        network_performance: "Up to 12.5 Gigabit",
        interfaces: (2, 4),
        bandwidth_weightings: CONFIGURABLE_WEIGHTINGS,
        efa_and_ena_srd: (false, false),
        hibernation_and_recovery: (true, true),
        host_features: ("supported", "supported", "unsupported", "unsupported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.8,
    },
    CatalogRow {
        instance_type: "r8g.16xlarge",
        ebs: (20_000, 80_000, 20_000, 80_000),
        attachments: (48, "dedicated"),
        network_performance: "30 Gigabit",
        interfaces: (15, 50),
        bandwidth_weightings: CONFIGURABLE_WEIGHTINGS,
        efa_and_ena_srd: (false, true),
        hibernation_and_recovery: (false, true),
        host_features: ("supported", "supported", "supported", "unsupported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.8,
    },
    CatalogRow {
        instance_type: "r8g.48xlarge",
        ebs: (40_000, 240_000, 40_000, 240_000),
        attachments: (128, "dedicated"),
        network_performance: "50 Gigabit",
        interfaces: (15, 50),
        bandwidth_weightings: CONFIGURABLE_WEIGHTINGS,
        efa_and_ena_srd: (true, true),
        hibernation_and_recovery: (false, true),
        host_features: ("supported", "supported", "supported", "unsupported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.7,
    },
    CatalogRow {
        instance_type: "r8g.metal-24xl",
        ebs: (30_000, 120_000, 30_000, 120_000),
        attachments: (39, "dedicated"),
        network_performance: "40 Gigabit",
        interfaces: (15, 50),
        bandwidth_weightings: CONFIGURABLE_WEIGHTINGS,
        efa_and_ena_srd: (true, true),
        hibernation_and_recovery: (false, true),
        host_features: ("supported", "supported", "unsupported", "unsupported"),
        boot_modes: &["uefi"],
        sustained_ghz: 2.8,
    },
];

/// Rule: the EBS performance and attachment limit a reported type carries are that type's own
/// published figures, and the two throughput members are the two bandwidth ones in MB/s.
#[tokio::test]
async fn published_ebs_figures_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();
    for row in ROWS {
        let case = row.instance_type;
        let body = described(&router, case).await;
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
        for (name, mbps) in [
            ("baselineThroughputInMBps", baseline_mbps),
            ("maximumThroughputInMBps", maximum_mbps),
        ] {
            let expected = mbps as f64 / MEGABITS_PER_MEGABYTE;
            let reported = number(case, &performance, name);
            assert!(
                (reported - expected).abs() < TOLERANCE,
                "case {case:?}: expected {name} {expected} (the bandwidth in the other \
                 unit), got {reported}"
            );
        }
        let (attachments, limit_type) = row.attachments;
        let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
        assert_eq!(
            reported, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {reported}"
        );
        let reported = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            reported, limit_type,
            "case {case:?}: expected the {limit_type:?} attachment limit kind, got {reported:?}"
        );
    }
}

/// Rule: the network figures and adapter capabilities a reported type carries are that type's
/// own published ones, including the elements the live wire renders beside the modelled ones.
#[tokio::test]
async fn published_network_figures_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();
    for row in ROWS {
        let case = row.instance_type;
        let body = described(&router, case).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "networkPerformance");
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );
        let (interfaces, addresses) = row.interfaces;
        for (name, expected) in [
            ("maximumNetworkInterfaces", interfaces),
            ("ipv4AddressesPerInterface", addresses),
            ("maximumNetworkCards", 1),
            ("defaultNetworkCardIndex", 0),
        ] {
            let reported = number(case, &network_info, name) as i64;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {name} {expected}, got {reported}"
            );
        }
        let weightings = element(&network_info, "bandwidthWeightings")
            .map(|reported| elements(&reported, "item"))
            .unwrap_or_default();
        assert_eq!(
            weightings,
            owned(row.bandwidth_weightings),
            "case {case:?}: expected the published bandwidth weightings"
        );
        let (efa, ena_srd) = row.efa_and_ena_srd;
        for (name, expected) in [("efaSupported", efa), ("enaSrdSupported", ena_srd)] {
            let reported = required(case, &network_info, name);
            assert_eq!(
                reported,
                expected.to_string(),
                "case {case:?}: expected {name} {expected}, got {reported:?}"
            );
        }
        assert_eq!(
            element(&network_info, "maximumEfaInterfaces").is_some(),
            efa,
            "case {case:?}: an EFA interface count is reported exactly for a type offered \
             one, got {network_info}"
        );
        let (_, _, _, mirroring) = row.host_features;
        let reported = required(case, &network_info, "trafficMirroringSupport");
        assert_eq!(
            reported, mirroring,
            "case {case:?}: expected trafficMirroringSupport {mirroring:?}, got {reported:?}"
        );
    }
}

/// Rule: the host features, boot modes and processor frequency a reported type carries are
/// that type's own published ones rather than a function of its family and its size.
#[tokio::test]
async fn published_host_features_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();
    for row in ROWS {
        let case = row.instance_type;
        let body = described(&router, case).await;
        let (hibernation, recovery) = row.hibernation_and_recovery;
        for (name, expected) in [
            ("hibernationSupported", hibernation),
            ("autoRecoverySupported", recovery),
        ] {
            let reported = required(case, &body, name);
            assert_eq!(
                reported,
                expected.to_string(),
                "case {case:?}: expected {name} {expected}, got {reported:?}"
            );
        }
        let (reboot_migration, phc, enclaves, _) = row.host_features;
        for (name, expected) in [
            ("rebootMigrationSupport", reboot_migration),
            ("phcSupport", phc),
            ("nitroEnclavesSupport", enclaves),
        ] {
            let reported = required(case, &body, name);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {name} {expected:?}, got {reported:?}"
            );
        }
        assert_eq!(
            list(case, &body, "supportedBootModes"),
            owned(row.boot_modes),
            "case {case:?}: expected the published boot modes"
        );
        let processor_info = required(case, &body, "processorInfo");
        let reported = number(case, &processor_info, "sustainedClockSpeedInGhz");
        assert!(
            (reported - row.sustained_ghz).abs() < TOLERANCE,
            "case {case:?}: expected sustainedClockSpeedInGhz {}, got {reported}",
            row.sustained_ghz
        );
        let reported = required(case, &body, "cpuBurstModel");
        assert_eq!(
            reported, NO_BURST_MODEL,
            "case {case:?}: a type whose vCPUs run at their full rate reports no burst \
             model, got {reported:?}"
        );
    }
}

/// Rule, as an invariant rather than a row: a published figure is a fact of the ONE instance
/// type, so one size of three families — one AMD x86, one Graviton2, one Graviton4 — reports
/// three different sets of them, which no function of the size multiplier could do.
#[tokio::test]
async fn one_size_of_three_families_reports_different_published_figures() {
    let (router, _engine) = super::ec2_query::emulator();
    let mut seen: Vec<(&str, Vec<String>)> = Vec::new();
    for case in ["c6a.16xlarge", "c6g.16xlarge", "r8g.16xlarge"] {
        let body = described(&router, case).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        let network_info = required(case, &body, "networkInfo");
        let processor_info = required(case, &body, "processorInfo");
        let figures = vec![
            required(case, &performance, "baselineBandwidthInMbps"),
            required(case, &network_info, "networkPerformance"),
            required(case, &ebs_info, "maximumEbsAttachments"),
            required(case, &processor_info, "sustainedClockSpeedInGhz"),
        ];
        if let Some((earlier, shared)) = seen.iter().find(|(_, other)| *other == figures) {
            panic!(
                "case {case:?}: one size of two families must not share its published \
                 figures, got {figures:?} which {earlier:?} already carries as {shared:?}"
            );
        }
        seen.push((case, figures));
    }
}

/// Rule: `DescribeInstanceTypes` reports a FACT of the catalog rather than a resource, so
/// reading one type twice reports it identically — a read changes nothing a later caller sees.
#[tokio::test]
async fn two_reads_of_one_instance_type_report_it_identically() {
    let (router, _engine) = super::ec2_query::emulator();
    for case in ["c6a.2xlarge", "c6g.metal", "r8g.medium", "r8g.metal-24xl"] {
        let first = described(&router, case).await;
        let second = described(&router, case).await;
        let reported = |body: &str| -> Vec<String> {
            let ebs_info = required(case, body, "ebsInfo");
            vec![
                required(case, &ebs_info, "maximumEbsAttachments"),
                required(case, &ebs_info, "attachmentLimitType"),
                required(case, body, "hibernationSupported"),
            ]
        };
        assert_eq!(
            reported(&first),
            reported(&second),
            "case {case:?}: two reads of one instance type must report it identically"
        );
    }
}
