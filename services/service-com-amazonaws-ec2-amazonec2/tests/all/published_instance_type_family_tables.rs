// SPDX-License-Identifier: CC0-1.0

//! The published facts of four instance-type FAMILIES of the catalog, driven over the wire
//! this service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with
//! XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one `published_instance_type_hardware_facts` pins for the
//! hardware members, applied to every member AWS publishes PER INSTANCE TYPE: the EBS
//! performance and attachment limit kind (`EbsInfo`), the network performance, bandwidth
//! weightings and adapter capabilities (`NetworkInfo`), the local devices
//! (`InstanceStorageInfo`), the processor's sustained frequency (`ProcessorInfo`), the core
//! counts a caller may configure (`VCpuInfo$ValidCores`), the boot modes, whether the type is
//! the latest generation of its family and whether it is offered on a Dedicated Host. Each of
//! those is the figure AWS publishes for the ONE type, so no function of the size multiplier
//! in the type's name produces it: two sizes of one family share a figure where the published
//! table says so, the device SIZE of a family's local store changes while the count grows, and
//! two families at one size agree on nothing.
//!
//! Every table spans what the rule generalises over — four families (two Intel, one Graviton
//! with local NVMe, one with configurable bandwidth), the smallest, a middle, the largest
//! virtualized and the bare metal size of each, a family with local devices and two without —
//! so no row is the specification and the sizes any one report happened to name are rows among
//! many.

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

/// The published facts of one size of a family.
#[derive(Debug, Clone, Copy)]
struct FamilyRow {
    /// `c5.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// Its network performance as the published table writes it, where the catalog records
    /// that figure for the size.
    network_performance: Option<&'static str>,
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its processor's sustained frequency, in GHz, where the catalog records it.
    sustained_ghz: Option<f64>,
    /// Is it the latest generation of its family?
    current_generation: bool,
    /// Is it offered on a Dedicated Host?
    dedicated_hosts: bool,
}

/// Rule: every member AWS publishes per instance type is served from that type's own row.
#[tokio::test]
async fn published_family_facts_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: the smallest, the size at which the family's bandwidth stops bursting, a middle
    // size, the largest virtualized size and the bare metal size of a compute optimized Intel
    // family; three sizes and the bare metal size of a general purpose Intel family; four
    // sizes of a Graviton family with local NVMe; and the bare metal size of a family with
    // configurable bandwidth, whose local devices and EBS figures are its own.
    let rows = [
        FamilyRow {
            instance_type: "c5.large",
            ebs: (650, 4_000, 4_750, 20_000),
            network_performance: Some("Up to 10 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.4),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "c5.4xlarge",
            ebs: (4_750, 20_000, 4_750, 20_000),
            network_performance: Some("Up to 10 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.4),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "c5.9xlarge",
            ebs: (9_500, 40_000, 9_500, 40_000),
            network_performance: Some("12 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.4),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "c5.12xlarge",
            ebs: (9_500, 40_000, 9_500, 40_000),
            network_performance: Some("12 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.6),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "c5.24xlarge",
            ebs: (19_000, 80_000, 19_000, 80_000),
            network_performance: Some("25 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.6),
            current_generation: false,
            dedicated_hosts: false,
        },
        FamilyRow {
            instance_type: "c5.metal",
            ebs: (19_000, 80_000, 19_000, 80_000),
            network_performance: Some("25 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.6),
            current_generation: false,
            dedicated_hosts: false,
        },
        FamilyRow {
            instance_type: "m6i.large",
            ebs: (625, 3_600, 10_000, 40_000),
            network_performance: Some("Up to 12.5 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.5),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "m6i.8xlarge",
            ebs: (10_000, 40_000, 10_000, 40_000),
            network_performance: Some("12.5 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.5),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "m6i.24xlarge",
            ebs: (30_000, 120_000, 30_000, 120_000),
            network_performance: Some("37.5 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.5),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "m6i.metal",
            ebs: (40_000, 160_000, 40_000, 160_000),
            network_performance: Some("50 Gigabit"),
            instance_store: None,
            sustained_ghz: Some(3.5),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "r7gd.medium",
            ebs: (315, 2_500, 10_000, 40_000),
            network_performance: Some("Up to 12.5 Gigabit"),
            instance_store: Some((1, 59)),
            sustained_ghz: Some(2.6),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "r7gd.4xlarge",
            ebs: (5_000, 20_000, 10_000, 40_000),
            network_performance: Some("Up to 15 Gigabit"),
            instance_store: Some((1, 950)),
            sustained_ghz: Some(2.6),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "r7gd.12xlarge",
            ebs: (15_000, 60_000, 15_000, 60_000),
            network_performance: Some("22.5 Gigabit"),
            instance_store: Some((2, 1_425)),
            sustained_ghz: Some(2.6),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "r7gd.metal",
            ebs: (20_000, 80_000, 20_000, 80_000),
            network_performance: Some("30 Gigabit"),
            instance_store: Some((2, 1_900)),
            sustained_ghz: Some(2.6),
            current_generation: false,
            dedicated_hosts: true,
        },
        FamilyRow {
            instance_type: "c9gd.metal-48xl",
            ebs: (72_000, 288_000, 72_000, 288_000),
            network_performance: None,
            instance_store: Some((3, 3_800)),
            sustained_ghz: None,
            current_generation: true,
            dedicated_hosts: true,
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
        // The two throughput members are the bandwidth ones in the other unit, and each is a
        // modelled double, so each carries a fractional part on the wire even where the value
        // is whole.
        for (name, bandwidth) in [
            ("baselineThroughputInMBps", baseline_mbps),
            ("maximumThroughputInMBps", maximum_mbps),
        ] {
            let text = required(case, &performance, name);
            assert!(
                text.contains('.'),
                "case {case:?}: {name} is a modelled double, so it carries a fractional \
                 part, got {text:?}"
            );
            let expected = bandwidth as f64 / MEGABITS_PER_MEGABYTE;
            let reported = number(case, &performance, name);
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected {name} {expected}, got {reported}"
            );
        }

        let storage = element(&body, "instanceStorageInfo");
        match row.instance_store {
            Some((devices, gb_each)) => {
                let storage = storage.unwrap_or_else(|| {
                    panic!("case {case:?}: expected an instanceStorageInfo, got {body}")
                });
                let disks = required(case, &storage, "disks");
                for (name, expected) in [("count", devices), ("sizeInGB", gb_each)] {
                    let reported = number(case, &disks, name) as i64;
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected local device {name} {expected}, got {reported}"
                    );
                }
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
        if let Some(expected) = row.network_performance {
            let reported = required(case, &network_info, "networkPerformance");
            assert_eq!(
                reported, expected,
                "case {case:?}: expected networkPerformance {expected:?}, got {reported:?}"
            );
        }
        if let Some(expected) = row.sustained_ghz {
            let processor_info = required(case, &body, "processorInfo");
            let reported = number(case, &processor_info, "sustainedClockSpeedInGhz");
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected sustainedClockSpeedInGhz {expected}, got {reported}"
            );
        }
        for (name, expected) in [
            ("currentGeneration", row.current_generation),
            ("dedicatedHostsSupported", row.dedicated_hosts),
        ] {
            let reported = required(case, &body, name);
            assert_eq!(
                reported,
                expected.to_string(),
                "case {case:?}: expected {name} {expected}, got {reported:?}"
            );
        }
    }
}

/// Rule, the other way round: the published figures are NOT a function of the size multiplier
/// in the type's name.
///
/// Four claims a formula over the name cannot satisfy at once: two sizes of one family that
/// share one EBS figure though their vCPUs differ, two more that share another, a family whose
/// local device SIZE changes while the count grows, and two families at one size that agree on
/// neither their EBS figure nor their network performance.
#[tokio::test]
async fn the_published_family_figures_do_not_follow_the_size_multiplier() {
    let (router, _engine) = super::ec2_query::emulator();

    let baseline = |body: &str, case: &str| -> i64 {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        number(case, &performance, "baselineBandwidthInMbps") as i64
    };

    for pair in [
        ["c5.9xlarge", "c5.12xlarge"],
        ["c5.18xlarge", "c5.24xlarge"],
    ] {
        let mut figures = Vec::new();
        for instance_type in pair {
            let body = described(instance_type, &router, instance_type).await;
            figures.push((
                instance_type,
                number(instance_type, &body, "defaultVCpus") as i64,
                baseline(&body, instance_type),
            ));
        }
        assert_eq!(
            figures[0].2, figures[1].2,
            "both sizes publish one sustained EBS figure, got {figures:?}"
        );
        assert!(
            figures[0].1 < figures[1].1,
            "the two sizes must differ in vCPUs, got {figures:?}"
        );
    }

    // One family, two sizes: the device size FALLS while the count grows, which no multiplier
    // over the name produces.
    let mut devices = Vec::new();
    for instance_type in ["r7gd.8xlarge", "r7gd.12xlarge"] {
        let body = described(instance_type, &router, instance_type).await;
        let storage = required(instance_type, &body, "instanceStorageInfo");
        let disks = required(instance_type, &storage, "disks");
        devices.push((
            instance_type,
            number(instance_type, &disks, "count") as i64,
            number(instance_type, &disks, "sizeInGB") as i64,
        ));
    }
    assert!(
        devices[0].1 < devices[1].1 && devices[0].2 > devices[1].2,
        "the larger size carries more, smaller devices, got {devices:?}"
    );

    // Two families, ONE size: neither figure agrees, so the size cannot be what produced
    // either.
    let left = described("c5.12xlarge", &router, "c5.12xlarge").await;
    let right = described("m6i.12xlarge", &router, "m6i.12xlarge").await;
    assert_ne!(
        baseline(&left, "c5.12xlarge"),
        baseline(&right, "m6i.12xlarge"),
        "two families of one size publish different EBS figures"
    );
    let left_network = required("c5.12xlarge", &left, "networkInfo");
    let right_network = required("m6i.12xlarge", &right, "networkInfo");
    assert_ne!(
        required("c5.12xlarge", &left_network, "networkPerformance"),
        required("m6i.12xlarge", &right_network, "networkPerformance"),
        "two families of one size publish different network performance"
    );
}

/// Rule: whether a type's EBS volumes have a limit of their own, and which bandwidth
/// weightings it can be configured with, are facts of the type.
#[tokio::test]
async fn attachment_limit_kind_and_bandwidth_weightings_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: four sizes of the family whose EBS devices have hardware of their own and whose
    // bandwidth split is configurable, and three types from families whose volumes come out of
    // the shared pool and whose split is fixed — which publish no weightings at all.
    let rows = [
        ("c9gd.medium", true, "dedicated"),
        ("c9gd.8xlarge", true, "dedicated"),
        ("c9gd.48xlarge", true, "dedicated"),
        ("c9gd.metal-48xl", true, "dedicated"),
        ("c5.large", false, "shared"),
        ("m6i.32xlarge", false, "shared"),
        ("r7gd.metal", false, "shared"),
    ];

    for (instance_type, weighted, limit_type) in rows {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let reported = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            reported, limit_type,
            "case {case:?}: expected attachmentLimitType {limit_type:?}, got {reported:?}"
        );
        let network_info = required(case, &body, "networkInfo");
        let reported = element(&network_info, "bandwidthWeightings");
        if weighted {
            let weightings = list(case, &network_info, "bandwidthWeightings");
            assert_eq!(
                weightings,
                vec!["default", "ebs-1", "vpc-1"],
                "case {case:?}: expected the published bandwidth weightings, got {weightings:?}"
            );
        } else {
            assert!(
                reported.is_none(),
                "case {case:?}: a type whose bandwidth split is fixed publishes no \
                 bandwidthWeightings, got {reported:?}"
            );
        }
    }

    // The EBS volume limit of the bare metal size whose figure IS published is that figure,
    // which is far above what the shared pool leaves any type.
    let case = "c9gd.metal-48xl";
    let body = described(case, &router, case).await;
    let ebs_info = required(case, &body, "ebsInfo");
    let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
    assert_eq!(
        reported, 79,
        "case {case:?}: expected the published maximumEbsAttachments 79, got {reported}"
    );
}

/// Rule: the adapter capabilities, the boot modes, the recovery flag and the core counts a
/// caller may configure are published per type too.
#[tokio::test]
async fn adapter_boot_and_core_facts_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: one flag per claim, each read on a type that has it and one that does not — a
    // configurable ENA queue count offered on one Intel family and not the older one, ENA
    // Express offered from the size that carries its family's whole network share up, the
    // Elastic Fabric Adapter only on the sizes that hold the whole host, and CloudWatch action
    // based recovery on the bare metal sizes that publish it.
    let flag_rows = [
        ("c5.large", "flexibleEnaQueuesSupport", "unsupported"),
        ("c5.24xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("m6i.large", "flexibleEnaQueuesSupport", "supported"),
        ("m6i.metal", "flexibleEnaQueuesSupport", "supported"),
        ("c5.24xlarge", "enaSrdSupported", "false"),
        ("m6i.2xlarge", "enaSrdSupported", "false"),
        ("m6i.16xlarge", "enaSrdSupported", "true"),
        ("m6i.24xlarge", "efaSupported", "false"),
        ("m6i.32xlarge", "efaSupported", "true"),
        ("m6i.metal", "efaSupported", "true"),
        ("r7gd.8xlarge", "efaSupported", "false"),
        ("r7gd.16xlarge", "efaSupported", "true"),
        ("r7gd.metal", "efaSupported", "true"),
        ("c5.metal", "autoRecoverySupported", "true"),
        ("m6i.metal", "autoRecoverySupported", "true"),
        ("r7gd.metal", "autoRecoverySupported", "false"),
        ("c5.metal", "rebootMigrationSupport", "supported"),
        ("c5.24xlarge", "rebootMigrationSupport", "supported"),
    ];

    for (instance_type, member, expected) in flag_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        // A member of networkInfo is read out of that block; the rest are the type's own.
        let scope = if member == "autoRecoverySupported" || member == "rebootMigrationSupport" {
            body
        } else {
            required(&case, &body, "networkInfo")
        };
        let reported = required(&case, &scope, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The boot modes of a bare metal size are its own: the Intel ones below are offered the
    // legacy BIOS mode alone where their virtualized sizes are offered both, and a Graviton
    // type is offered UEFI only.
    let boot_rows = [
        ("c5.metal", vec!["legacy-bios"]),
        ("c5.24xlarge", vec!["legacy-bios", "uefi"]),
        ("m6i.metal", vec!["legacy-bios"]),
        ("m6i.32xlarge", vec!["legacy-bios", "uefi"]),
        ("r7gd.metal", vec!["uefi"]),
    ];

    for (instance_type, expected) in boot_rows {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = list(case, &body, "supportedBootModes");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {reported:?}"
        );
    }

    // The core counts: the sizes of the two Intel families whose cores are configured in twos
    // publish the even counts up to their default — a size with one core alone taking that one
    // count — while their bare metal size and a Graviton type publish every count.
    let core_rows = [
        ("c5.large", vec![1]),
        ("c5.xlarge", vec![2]),
        ("c5.2xlarge", vec![2, 4]),
        ("c5.4xlarge", vec![2, 4, 6, 8]),
        ("c5.12xlarge", (1..=12).map(|step| step * 2).collect()),
        ("c5.metal", (1..=48).collect::<Vec<i64>>()),
        ("m6i.xlarge", vec![1, 2]),
        ("m6i.4xlarge", vec![2, 4, 6, 8]),
        ("m6i.metal", (1..=64).collect()),
        ("r7gd.xlarge", vec![1, 2, 3, 4]),
        ("r7gd.4xlarge", (1..=16).collect()),
    ];

    for (instance_type, expected) in core_rows {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let v_cpu_info = required(case, &body, "vCpuInfo");
        let reported: Vec<i64> = list(case, &v_cpu_info, "validCores")
            .iter()
            .map(|count| {
                count.parse().unwrap_or_else(|_| {
                    panic!("case {case:?}: a core count must be a number, got {count:?}")
                })
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected validCores {expected:?}, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Two more compute optimized families: one Intel, one Graviton
// ---------------------------------------------------------------------------

/// The published facts of one size of a compute optimized family.
///
/// Each figure is the one AWS publishes for that ONE type: the four EBS figures and the
/// attachment limit come from "Amazon EBS optimized instance types" and "Instance volume
/// limits", the network performance from the family's own specification page, and the core
/// counts from the published list of the counts a type can be configured with.
#[derive(Debug, Clone)]
struct ComputeRow {
    /// `c6i.8xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// How many Amazon EBS volumes the type can hold attached.
    ebs_attachments: i64,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its processor's sustained frequency, in GHz.
    sustained_ghz: f64,
    /// Whether a configurable number of ENA queues is offered, as the enum spells it.
    flexible_ena_queues: &'static str,
    /// Whether ENA Express is offered.
    ena_srd: bool,
    /// The Elastic Fabric Adapters it is offered, zero where it is offered none.
    efa_interfaces: i64,
    /// The core counts a caller may configure.
    valid_cores: Vec<i64>,
}

/// The rows of the two families, in size order within each.
fn compute_rows() -> Vec<ComputeRow> {
    let evens_to = |cores: i64| (1..=cores / 2).map(|step| step * 2).collect::<Vec<i64>>();
    let every_count_to = |cores: i64| (1..=cores).collect::<Vec<i64>>();
    vec![
        // C6i: the smallest size, the first size whose cores go up in twos, the size at which
        // the bandwidth stops bursting, a middle size, the largest virtualized size and the
        // bare metal one.
        ComputeRow {
            instance_type: "c6i.large",
            ebs: (650, 3_600, 10_000, 40_000),
            ebs_attachments: 27,
            network_performance: "Up to 12.5 Gigabit",
            sustained_ghz: 3.5,
            flexible_ena_queues: "supported",
            ena_srd: false,
            efa_interfaces: 0,
            valid_cores: vec![1],
        },
        ComputeRow {
            instance_type: "c6i.2xlarge",
            ebs: (2_500, 12_000, 10_000, 40_000),
            ebs_attachments: 27,
            network_performance: "Up to 12.5 Gigabit",
            sustained_ghz: 3.5,
            flexible_ena_queues: "supported",
            ena_srd: false,
            efa_interfaces: 0,
            valid_cores: vec![2, 4],
        },
        ComputeRow {
            instance_type: "c6i.8xlarge",
            ebs: (10_000, 40_000, 10_000, 40_000),
            ebs_attachments: 27,
            network_performance: "12.5 Gigabit",
            sustained_ghz: 3.5,
            flexible_ena_queues: "supported",
            ena_srd: true,
            efa_interfaces: 0,
            valid_cores: evens_to(16),
        },
        ComputeRow {
            instance_type: "c6i.16xlarge",
            ebs: (20_000, 80_000, 20_000, 80_000),
            ebs_attachments: 27,
            network_performance: "25 Gigabit",
            sustained_ghz: 3.5,
            flexible_ena_queues: "supported",
            ena_srd: true,
            efa_interfaces: 0,
            valid_cores: evens_to(32),
        },
        ComputeRow {
            instance_type: "c6i.32xlarge",
            ebs: (40_000, 160_000, 40_000, 160_000),
            ebs_attachments: 27,
            network_performance: "50 Gigabit",
            sustained_ghz: 3.5,
            flexible_ena_queues: "supported",
            ena_srd: true,
            efa_interfaces: 1,
            valid_cores: evens_to(64),
        },
        ComputeRow {
            instance_type: "c6i.metal",
            ebs: (40_000, 160_000, 40_000, 160_000),
            ebs_attachments: 31,
            network_performance: "50 Gigabit",
            sustained_ghz: 3.5,
            flexible_ena_queues: "unsupported",
            ena_srd: true,
            efa_interfaces: 1,
            valid_cores: every_count_to(64),
        },
        // C7g: the smallest size of the Graviton3 ladder, a size below the one that stops
        // bursting, that size itself, the largest virtualized size and the bare metal one.
        ComputeRow {
            instance_type: "c7g.medium",
            ebs: (315, 2_500, 10_000, 40_000),
            ebs_attachments: 27,
            network_performance: "Up to 12.5 Gigabit",
            sustained_ghz: 2.6,
            flexible_ena_queues: "unsupported",
            ena_srd: false,
            efa_interfaces: 0,
            valid_cores: vec![1],
        },
        ComputeRow {
            instance_type: "c7g.xlarge",
            ebs: (1_250, 6_000, 10_000, 40_000),
            ebs_attachments: 27,
            network_performance: "Up to 12.5 Gigabit",
            sustained_ghz: 2.6,
            flexible_ena_queues: "unsupported",
            ena_srd: false,
            efa_interfaces: 0,
            valid_cores: every_count_to(4),
        },
        ComputeRow {
            instance_type: "c7g.8xlarge",
            ebs: (10_000, 40_000, 10_000, 40_000),
            ebs_attachments: 27,
            network_performance: "15 Gigabit",
            sustained_ghz: 2.6,
            flexible_ena_queues: "unsupported",
            ena_srd: false,
            efa_interfaces: 0,
            valid_cores: every_count_to(32),
        },
        ComputeRow {
            instance_type: "c7g.16xlarge",
            ebs: (20_000, 80_000, 20_000, 80_000),
            ebs_attachments: 27,
            network_performance: "30 Gigabit",
            sustained_ghz: 2.6,
            flexible_ena_queues: "unsupported",
            ena_srd: true,
            efa_interfaces: 1,
            valid_cores: every_count_to(64),
        },
        ComputeRow {
            instance_type: "c7g.metal",
            ebs: (20_000, 80_000, 20_000, 80_000),
            ebs_attachments: 31,
            network_performance: "30 Gigabit",
            sustained_ghz: 2.6,
            flexible_ena_queues: "unsupported",
            ena_srd: true,
            efa_interfaces: 1,
            valid_cores: every_count_to(64),
        },
    ]
}

/// Rule: every member AWS publishes per instance type is served from that type's own row —
/// here over two more compute optimized families, one Intel and one Graviton.
#[tokio::test]
async fn the_two_compute_families_serve_their_published_ebs_network_and_core_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in compute_rows() {
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
        // The throughput members are the bandwidth ones in the other unit, which is exactly
        // how the published table's Mbps and MB/s columns relate.
        for (name, bandwidth) in [
            ("baselineThroughputInMBps", baseline_mbps),
            ("maximumThroughputInMBps", maximum_mbps),
        ] {
            let expected = bandwidth as f64 / MEGABITS_PER_MEGABYTE;
            let reported = number(case, &performance, name);
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected {name} {expected}, got {reported} \
                 ({:?} on the wire)",
                required(case, &performance, name)
            );
        }
        let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
        assert_eq!(
            reported, row.ebs_attachments,
            "case {case:?}: expected maximumEbsAttachments {}, got {reported}",
            row.ebs_attachments
        );

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

        let v_cpu_info = required(case, &body, "vCpuInfo");
        let reported: Vec<i64> = list(case, &v_cpu_info, "validCores")
            .iter()
            .map(|count| {
                count.parse().unwrap_or_else(|_| {
                    panic!("case {case:?}: a core count must be a number, got {count:?}")
                })
            })
            .collect();
        assert_eq!(
            reported, row.valid_cores,
            "case {case:?}: expected validCores {:?}, got {reported:?}",
            row.valid_cores
        );
    }
}

/// Rule: the adapter capabilities of those two families are published per type as well — a
/// configurable ENA queue count, ENA Express and the Elastic Fabric Adapter each reported for
/// the sizes that are offered it and against the sizes that are not.
#[tokio::test]
async fn the_two_compute_families_serve_their_published_adapter_capabilities() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in compute_rows() {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        for (name, expected) in [
            ("flexibleEnaQueuesSupport", row.flexible_ena_queues.to_string()),
            ("enaSrdSupported", row.ena_srd.to_string()),
            ("efaSupported", (row.efa_interfaces > 0).to_string()),
            // One physical network card on every size of both families, and the first card is
            // the default.
            ("maximumNetworkCards", "1".to_string()),
            ("defaultNetworkCardIndex", "0".to_string()),
        ] {
            let reported = required(case, &network_info, name);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {name} {expected:?}, got {reported:?}"
            );
        }
        let efa_info = element(&network_info, "efaInfo");
        if row.efa_interfaces > 0 {
            let efa_info = efa_info.unwrap_or_else(|| {
                panic!("case {case:?}: a type offered the adapter describes it, got {body}")
            });
            let reported = number(case, &efa_info, "maximumEfaInterfaces") as i64;
            assert_eq!(
                reported, row.efa_interfaces,
                "case {case:?}: expected maximumEfaInterfaces {}, got {reported}",
                row.efa_interfaces
            );
        } else {
            assert!(
                efa_info.is_none(),
                "case {case:?}: a type offered no Elastic Fabric Adapter describes none, got \
                 {efa_info:?}"
            );
        }
        // Neither hardware generation carries a local PTP hardware clock, and both are
        // offered reboot migration — the bare metal sizes too, the move to another host
        // being the host's to make rather than the hypervisor's they have none of.
        for (name, expected) in [
            ("phcSupport", "unsupported"),
            ("rebootMigrationSupport", "supported"),
        ] {
            let reported = required(case, &body, name);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {name} {expected:?}, got {reported:?}"
            );
        }
    }

    // The Graviton3 family takes no interface attachment from a secondary network, so no
    // address at all is available on one, while the Intel family reports the support alone —
    // two answers one flag over the family cannot give.
    for (instance_type, secondary_addresses) in [
        ("c7g.medium", Some("0")),
        ("c7g.metal", Some("0")),
        ("c6i.large", None),
        ("c6i.metal", None),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "secondaryNetworkSupported");
        assert_eq!(
            reported, "false",
            "case {case:?}: expected secondaryNetworkSupported false, got {reported:?}"
        );
        let addresses = element(&network_info, "ipv4AddressesPerSecondaryInterface");
        assert_eq!(
            addresses.as_deref(),
            secondary_addresses,
            "case {case:?}: expected ipv4AddressesPerSecondaryInterface \
             {secondary_addresses:?}, got {addresses:?}"
        );
    }

    // Two families at ONE size agree on neither their EBS figures nor their network
    // performance, so neither follows from the size in the name.
    let left = described("c6i.large", &router, "c6i.large").await;
    let right = described("c7g.large", &router, "c7g.large").await;
    let baseline = |body: &str, case: &str| -> i64 {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        number(case, &performance, "baselineBandwidthInMbps") as i64
    };
    assert_ne!(
        baseline(&left, "c6i.large"),
        baseline(&right, "c7g.large"),
        "two families of one size publish different sustained EBS bandwidth"
    );
    let left = described("c6i.16xlarge", &router, "c6i.16xlarge").await;
    let right = described("c7g.16xlarge", &router, "c7g.16xlarge").await;
    let left_network = required("c6i.16xlarge", &left, "networkInfo");
    let right_network = required("c7g.16xlarge", &right, "networkInfo");
    assert_ne!(
        required("c6i.16xlarge", &left_network, "networkPerformance"),
        required("c7g.16xlarge", &right_network, "networkPerformance"),
        "two families of one size publish different network performance"
    );
}
