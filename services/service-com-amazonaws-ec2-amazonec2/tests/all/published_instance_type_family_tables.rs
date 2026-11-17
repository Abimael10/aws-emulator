// SPDX-License-Identifier: CC0-1.0

//! The published facts of seven instance-type FAMILIES of the catalog, driven over the wire
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
//! Every table spans what the rule generalises over — seven families (four Intel, two
//! Graviton with local NVMe, one with configurable bandwidth), the smallest, a middle, the
//! largest virtualized and the bare metal size of each, families with local devices and
//! families without — so no row is the specification and the sizes any one report happened to
//! name are rows among many. A published figure that is not a whole number of Mbps is a row
//! among them too: the integer bandwidth member cannot hold its half while the throughput
//! member carries the megabytes of the whole figure.

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

/// The published row of one size of a family whose sizes carry local NVMe devices, or whose
/// network is the family's capability letter.
#[derive(Debug, Clone, Copy)]
struct LocalAndNetworkRow {
    /// `c6id.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// The EBS volumes it holds attached, and whether that limit is shared with its
    /// interfaces and local devices or is the volumes' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices as (count, GB each), or `None` where it carries none.
    instance_store: Option<(i64, i64)>,
    /// Its network performance as the published table writes it.
    network_performance: &'static str,
    /// Its processor's sustained frequency, in GHz.
    sustained_ghz: f64,
    /// The core counts it can be configured with: (smallest, step) of its published list,
    /// which runs up to its default core count.
    valid_cores: (i64, i64),
}

/// Three more families of the catalog, one row per size.
///
/// The figures are the ones AWS states for each type on its family's instance-type
/// specification page, in the "Amazon EBS optimized instance types" and "Instance volume
/// limits" tables, in the "IP addresses per network interface per instance type" table and in
/// the "CPU cores and threads per CPU core per instance type" table of the EC2 User Guide.
///
/// The rows span what the rule generalises over: a compute optimized Intel family with local
/// NVMe, a compute optimized Intel family built for the network and carrying none, and a
/// Graviton general purpose family with local NVMe; the smallest size, the sizes whose
/// published device size is NOT the quarter or the three quarters of the family's largest
/// device that a smooth ladder would give, a size that carries four devices, the largest
/// virtualized sizes and the bare metal size of each. So the sizes differ in device count,
/// in device size, in network performance, in processor frequency, in the attachment limit
/// they publish and in the core counts they offer.
const LOCAL_AND_NETWORK_ROWS: [LocalAndNetworkRow; 13] = [
    LocalAndNetworkRow {
        instance_type: "c6id.large",
        ebs: (650, 3_600, 10_000, 40_000),
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 118)),
        network_performance: "Up to 12.5 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "c6id.2xlarge",
        ebs: (2_500, 12_000, 10_000, 40_000),
        ebs_attachments: (26, "shared"),
        // 474 GB, not the 475 a quarter of the family's largest device would be.
        instance_store: Some((1, 474)),
        network_performance: "Up to 12.5 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (2, 2),
    },
    LocalAndNetworkRow {
        instance_type: "c6id.12xlarge",
        ebs: (15_000, 60_000, 15_000, 60_000),
        ebs_attachments: (25, "shared"),
        instance_store: Some((2, 1_425)),
        network_performance: "18.75 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (2, 2),
    },
    LocalAndNetworkRow {
        instance_type: "c6id.24xlarge",
        ebs: (30_000, 120_000, 30_000, 120_000),
        ebs_attachments: (23, "shared"),
        instance_store: Some((4, 1_425)),
        network_performance: "37.5 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (2, 2),
    },
    LocalAndNetworkRow {
        instance_type: "c6id.metal",
        ebs: (40_000, 160_000, 40_000, 160_000),
        ebs_attachments: (31, "shared"),
        instance_store: Some((4, 1_900)),
        network_performance: "50 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "c6in.xlarge",
        ebs: (3_125, 12_500, 25_000, 100_000),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "Up to 30 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "c6in.8xlarge",
        ebs: (25_000, 100_000, 25_000, 100_000),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "50 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "c6in.16xlarge",
        ebs: (50_000, 200_000, 50_000, 200_000),
        ebs_attachments: (27, "shared"),
        instance_store: None,
        network_performance: "100 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "c6in.metal",
        ebs: (100_000, 400_000, 100_000, 400_000),
        ebs_attachments: (31, "shared"),
        instance_store: None,
        network_performance: "200 Gigabit",
        sustained_ghz: 3.5,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "m7gd.medium",
        ebs: (315, 2_500, 10_000, 40_000),
        ebs_attachments: (26, "shared"),
        instance_store: Some((1, 59)),
        network_performance: "Up to 12.5 Gigabit",
        sustained_ghz: 2.6,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "m7gd.2xlarge",
        ebs: (2_500, 12_000, 10_000, 40_000),
        ebs_attachments: (26, "shared"),
        // 474 GB here too, on a family of a different architecture and a different ladder.
        instance_store: Some((1, 474)),
        network_performance: "Up to 15 Gigabit",
        sustained_ghz: 2.6,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "m7gd.12xlarge",
        ebs: (15_000, 60_000, 15_000, 60_000),
        ebs_attachments: (25, "shared"),
        instance_store: Some((2, 1_425)),
        network_performance: "22.5 Gigabit",
        sustained_ghz: 2.6,
        valid_cores: (1, 1),
    },
    LocalAndNetworkRow {
        instance_type: "m7gd.metal",
        ebs: (20_000, 80_000, 20_000, 80_000),
        ebs_attachments: (31, "shared"),
        instance_store: Some((2, 1_900)),
        network_performance: "30 Gigabit",
        sustained_ghz: 2.6,
        valid_cores: (1, 1),
    },
];

/// Rule: every hardware member AWS publishes per instance type is served from that type's own
/// row — the EBS figures, the attachment limit and its kind, the local devices, the network
/// performance, the processor frequency and the core counts.
#[tokio::test]
async fn the_local_and_network_families_serve_their_published_hardware_rows() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in LOCAL_AND_NETWORK_ROWS {
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

        let (attachments, limit_kind) = row.ebs_attachments;
        let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
        assert_eq!(
            reported, attachments,
            "case {case:?}: expected maximumEbsAttachments {attachments}, got {reported}"
        );
        let reported = required(case, &ebs_info, "attachmentLimitType");
        assert_eq!(
            reported, limit_kind,
            "case {case:?}: expected attachmentLimitType {limit_kind:?}, got {reported:?}"
        );

        match row.instance_store {
            Some((devices, gb_each)) => {
                let storage = required(case, &body, "instanceStorageInfo");
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
                element(&body, "instanceStorageInfo").is_none(),
                "case {case:?}: a type with no local devices reports no instanceStorageInfo, \
                 got {body}"
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

        let v_cpu_info = required(case, &body, "vCpuInfo");
        let default_cores = number(case, &v_cpu_info, "defaultCores") as i64;
        let (minimum, step) = row.valid_cores;
        let expected: Vec<i64> = (1..=default_cores)
            .filter(|cores| cores % step == 0 && *cores >= minimum)
            .collect();
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

/// Rule, the other way round: the published figures of these families are NOT a function of
/// the size multiplier in the name, so one size of two families agrees on nothing and one
/// family's ladder does not scale.
#[tokio::test]
async fn the_local_and_network_published_figures_do_not_follow_the_size_multiplier() {
    let (router, _engine) = super::ec2_query::emulator();

    let baseline = |body: &str, case: &str| -> i64 {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        number(case, &performance, "baselineBandwidthInMbps") as i64
    };
    let total_storage = |body: &str, case: &str| -> i64 {
        let storage = required(case, body, "instanceStorageInfo");
        number(case, &storage, "totalSizeInGB") as i64
    };

    // One size, two families that differ by a capability letter: the family built for the
    // network sustains eight times the EBS bandwidth of its plain sibling, and carries no
    // local device where the `d` sibling carries one.
    let case = "c6in.2xlarge";
    let network_built = described(case, &router, case).await;
    let local = described("c6id.2xlarge", &router, "c6id.2xlarge").await;
    assert!(
        baseline(&network_built, case) > baseline(&local, "c6id.2xlarge"),
        "case {case:?}: the network optimized family sustains more EBS bandwidth than the \
         local-storage one at the same size, got {} against {}",
        baseline(&network_built, case),
        baseline(&local, "c6id.2xlarge")
    );
    assert!(
        element(&network_built, "instanceStorageInfo").is_none(),
        "case {case:?}: a type with no local devices reports no instanceStorageInfo, got \
         {network_built}"
    );

    // The device SIZE of a ladder does not scale with the size: the 24xlarge carries four
    // devices smaller than the one device of the 8xlarge, so the total is not the multiplier
    // applied to either figure.
    let eight = described("c6id.8xlarge", &router, "c6id.8xlarge").await;
    let twenty_four = described("c6id.24xlarge", &router, "c6id.24xlarge").await;
    let eight_total = total_storage(&eight, "c6id.8xlarge");
    let twenty_four_total = total_storage(&twenty_four, "c6id.24xlarge");
    assert_eq!(
        (eight_total, twenty_four_total),
        (1_900, 5_700),
        "expected the published totals 1900 and 5700 GB, got {eight_total} and \
         {twenty_four_total}"
    );

    // Two architectures at one size publish different EBS baselines and different network
    // performance, so neither follows from the size in the name.
    let intel = described("c6id.2xlarge", &router, "c6id.2xlarge").await;
    let graviton = described("m7gd.2xlarge", &router, "m7gd.2xlarge").await;
    let intel_network = required("c6id.2xlarge", &intel, "networkInfo");
    let graviton_network = required("m7gd.2xlarge", &graviton, "networkInfo");
    assert_ne!(
        required("c6id.2xlarge", &intel_network, "networkPerformance"),
        required("m7gd.2xlarge", &graviton_network, "networkPerformance"),
        "two families of one size publish different network performance"
    );
}

/// Rule: the adapter capabilities, the host features, the Elastic Fabric Adapters and the
/// interface limits of these families are published per type too — none of them follows from
/// the family's generation alone.
#[tokio::test]
async fn the_local_and_network_families_serve_their_published_capabilities() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: (instance type, member, expected). A member of `networkInfo` is read out of that
    // block, the rest are the type's own.
    for (instance_type, member, expected) in [
        // ENA Express is offered from the size that carries a family's whole dedicated share
        // up, and on the bare metal size, not below it.
        ("c6id.large", "enaSrdSupported", "false"),
        ("c6id.12xlarge", "enaSrdSupported", "true"),
        ("c6id.metal", "enaSrdSupported", "true"),
        ("c6in.xlarge", "enaSrdSupported", "false"),
        ("c6in.8xlarge", "enaSrdSupported", "true"),
        ("c6in.metal", "enaSrdSupported", "true"),
        ("m7gd.medium", "enaSrdSupported", "false"),
        ("m7gd.12xlarge", "enaSrdSupported", "true"),
        ("m7gd.metal", "enaSrdSupported", "true"),
        // A configurable ENA queue count is the Intel families' adapter and not the
        // Graviton one's, and is not offered on a bare metal size of either.
        ("c6id.12xlarge", "flexibleEnaQueuesSupport", "supported"),
        ("c6id.metal", "flexibleEnaQueuesSupport", "unsupported"),
        ("c6in.8xlarge", "flexibleEnaQueuesSupport", "supported"),
        ("c6in.metal", "flexibleEnaQueuesSupport", "unsupported"),
        ("m7gd.12xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        // No size of any of the three takes an interface attachment from a secondary
        // network; the two families whose rows publish the addresses one such interface
        // takes report none at all, and the third reports no figure.
        ("c6id.12xlarge", "secondaryNetworkSupported", "false"),
        ("c6in.8xlarge", "ipv4AddressesPerSecondaryInterface", "0"),
        ("m7gd.medium", "ipv4AddressesPerSecondaryInterface", "0"),
        // The Elastic Fabric Adapter is offered on the sizes the family publishes it for and
        // on no other, however large.
        ("c6id.12xlarge", "efaSupported", "false"),
        ("c6id.metal", "efaSupported", "true"),
        ("c6in.16xlarge", "efaSupported", "false"),
        ("c6in.32xlarge", "efaSupported", "true"),
        ("m7gd.12xlarge", "efaSupported", "false"),
        ("m7gd.16xlarge", "efaSupported", "true"),
        // The smallest Graviton size takes the interfaces and addresses its row publishes
        // rather than the ones its step of the size ladder would give.
        ("m7gd.medium", "maximumNetworkInterfaces", "2"),
        ("m7gd.medium", "ipv4AddressesPerInterface", "4"),
        // The two sizes that hold the whole network optimized host carry two cards.
        ("c6in.32xlarge", "maximumNetworkCards", "2"),
        ("c6in.metal", "maximumNetworkCards", "2"),
        ("c6id.metal", "maximumNetworkCards", "1"),
    ] {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let network_info = required(&case, &body, "networkInfo");
        let reported = required(&case, &network_info, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The Elastic Fabric Adapters a type carries: one per network card on the sizes that
    // carry two, one on every other size that is offered any.
    for (instance_type, interfaces) in [
        ("c6id.metal", "1"),
        ("c6in.32xlarge", "2"),
        ("c6in.metal", "2"),
        ("m7gd.16xlarge", "1"),
        ("m7gd.metal", "1"),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let efa_info = required(case, &network_info, "efaInfo");
        let reported = required(case, &efa_info, "maximumEfaInterfaces");
        assert_eq!(
            reported, interfaces,
            "case {case:?}: expected maximumEfaInterfaces {interfaces:?}, got {reported:?}"
        );
    }

    // The host features are the type's own: a family whose local devices a migration cannot
    // carry over is offered no reboot migration at any size, while the family that carries
    // none is — on its bare metal size too, the move there being the host's to make. A local
    // PTP hardware clock is the Graviton host's own hardware, so its bare metal size has one
    // as well. Simplified automatic recovery reaches a bare metal size only where no local
    // device holds data a recovery would have to carry over. An enclave is offered from two
    // vCPUs up on the Graviton architecture and from four on an x86 one.
    for (instance_type, member, expected) in [
        ("c6id.12xlarge", "rebootMigrationSupport", "unsupported"),
        ("c6id.metal", "rebootMigrationSupport", "unsupported"),
        ("c6in.8xlarge", "rebootMigrationSupport", "supported"),
        ("c6in.metal", "rebootMigrationSupport", "supported"),
        ("m7gd.12xlarge", "rebootMigrationSupport", "unsupported"),
        ("c6id.12xlarge", "phcSupport", "unsupported"),
        ("c6in.8xlarge", "phcSupport", "unsupported"),
        ("m7gd.medium", "phcSupport", "supported"),
        ("m7gd.metal", "phcSupport", "supported"),
        ("c6in.metal", "autoRecoverySupported", "true"),
        ("c6id.metal", "autoRecoverySupported", "false"),
        ("m7gd.metal", "autoRecoverySupported", "false"),
        ("m7gd.medium", "nitroEnclavesSupport", "unsupported"),
        ("m7gd.large", "nitroEnclavesSupport", "supported"),
        ("c6id.large", "nitroEnclavesSupport", "unsupported"),
        ("c6id.xlarge", "nitroEnclavesSupport", "supported"),
    ] {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let reported = required(&case, &body, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The boot modes of a bare metal size are its own: the Intel ones are offered the legacy
    // BIOS mode alone where their virtualized sizes take both, and a Graviton type takes UEFI
    // at every size.
    for (instance_type, expected) in [
        ("c6id.metal", vec!["legacy-bios"]),
        ("c6in.metal", vec!["legacy-bios"]),
        ("c6id.12xlarge", vec!["legacy-bios", "uefi"]),
        ("m7gd.metal", vec!["uefi"]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = list(case, &body, "supportedBootModes");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {reported:?}"
        );
    }
}

/// Rule: the two members that report a type's published sustained EBS bandwidth are that ONE
/// published figure in two units — including where the figure is not a whole number of Mbps,
/// which the integer member cannot hold while the MB/s member carries the megabytes of the
/// whole figure — and a card's share of it is reported through the same two readings, so a
/// card and the instance it belongs to cannot report that figure differently.
#[tokio::test]
async fn a_published_sustained_bandwidth_is_reported_in_both_its_units() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: the published figure as each member reports it — the Mbps the integer member
    // holds, and the megabytes of the WHOLE published figure. The first size's published
    // share of its host is 1,562.5 Mbps, so the two members do not relate by a division of
    // the integer one; the rest are whole numbers of Mbps, the last of them with a fractional
    // megabyte figure. The figures come from the "Amazon EBS optimized instance types" table
    // of the EC2 User Guide.
    for (instance_type, bandwidth, throughput) in [
        ("c6in.large", 1_562_i64, 195.312_5_f64),
        ("c6in.xlarge", 3_125, 390.625),
        ("c6id.large", 650, 81.25),
        ("m7gd.medium", 315, 39.375),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        let reported = number(case, &performance, "baselineBandwidthInMbps") as i64;
        assert_eq!(
            reported, bandwidth,
            "case {case:?}: expected baselineBandwidthInMbps {bandwidth}, got {reported}"
        );
        let reported = number(case, &performance, "baselineThroughputInMBps");
        assert!(
            (reported - throughput).abs() < 1e-9,
            "case {case:?}: expected baselineThroughputInMBps {throughput}, got {reported}"
        );
        // These types' volumes are attached out of the pool they share with the instance's
        // other devices rather than to EBS hardware of the type's own, so there are no cards
        // to spread the figure over and none is reported.
        assert!(
            element(&ebs_info, "ebsCardSet").is_none()
                && element(&ebs_info, "maximumEbsCards").is_none(),
            "case {case:?}: a type whose volumes share the host's pool reports no EBS cards, \
             got {ebs_info}"
        );
    }

    // A type whose row DOES publish EBS hardware of its own: its cards carry the instance's
    // published figure together, each card reporting its share through the same two readings.
    let case = "c8gb.48xlarge";
    let body = described(case, &router, case).await;
    let ebs_info = required(case, &body, "ebsInfo");
    let performance = required(case, &ebs_info, "ebsOptimizedInfo");
    let instance_bandwidth = number(case, &performance, "baselineBandwidthInMbps");
    let cards = list(case, &ebs_info, "ebsCardSet");
    let reported = number(case, &ebs_info, "maximumEbsCards") as i64;
    assert_eq!(
        reported,
        cards.len() as i64,
        "case {case:?}: expected maximumEbsCards to be the {} cards reported, got {reported}",
        cards.len()
    );
    let mut carried = 0.0;
    for (index, card) in cards.iter().enumerate() {
        let case = format!("{case} card {index}");
        let bandwidth = number(&case, card, "baselineBandwidthInMbps");
        let throughput = number(&case, card, "baselineThroughputInMBps");
        assert!(
            (throughput - bandwidth / MEGABITS_PER_MEGABYTE).abs() < 1e-9,
            "case {case:?}: expected baselineThroughputInMBps {}, got {throughput}",
            bandwidth / MEGABITS_PER_MEGABYTE
        );
        carried += bandwidth;
    }
    assert!(
        (carried - instance_bandwidth).abs() < 1e-9,
        "case {case:?}: expected the cards to carry the instance's {instance_bandwidth} Mbps \
         together, got {carried}"
    );
}
