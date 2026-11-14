// SPDX-License-Identifier: CC0-1.0

//! The published per-type rows of a compute optimized family whose sizes carry local NVMe
//! devices — driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: EVERY HARDWARE MEMBER `DescribeInstanceTypes` REPORTS FOR AN INSTANCE
//! TYPE IS THE FIGURE AWS PUBLISHES FOR THAT ONE TYPE, not a function of the size multiplier
//! in its name. The EBS bandwidth and IOPS a type sustains and the ones it reaches
//! (`EbsInfo$EbsOptimizedInfo`), how many volumes it holds attached and whether that limit is
//! shared with its other devices (`$MaximumEbsAttachments` / `$AttachmentLimitType`), its
//! local devices (`InstanceStorageInfo$Disks` / `$TotalSizeInGB`), its network performance
//! (`NetworkInfo$NetworkPerformance`), its processor's sustained frequency
//! (`ProcessorInfo$SustainedClockSpeedInGhz`), the core counts it can be configured with
//! (`VCpuInfo$ValidCores`) and the host features it is offered (Dedicated Hosts, reboot
//! migration, a PTP hardware clock, Traffic Mirroring, the boot modes) are each a published
//! fact of the type.
//!
//! The table spans what the rule generalises over: every size of the ladder is a row — from
//! the smallest, through the two sizes that share one EBS figure, to the bare metal size —
//! so no one size is the specification, and the sizes differ in device count, in device
//! size, in processor frequency, in network performance and in the core counts they publish.
//! The figures are the ones AWS states for each type on its family's instance-type
//! specification page, in the "Amazon EBS optimized instance types" and "Instance volume
//! limits" tables and in the "CPU cores and threads per CPU core per instance type" table of
//! the EC2 User Guide.

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
    element(xml, name)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> reported, got {xml}"))
}

/// One member of a reported type as a number.
fn number(case: &str, xml: &str, name: &str) -> f64 {
    let text = required(case, xml, name);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: <{name}> must be a number, got {text:?}"))
}

/// The published row of one instance type: the figures a client sizing its storage, its EBS
/// throughput and its CPU options reads.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `c5d.9xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// The EBS volumes it holds attached, and whether that limit is shared with its
    /// interfaces and local devices or is the volumes' own.
    ebs_attachments: (i64, &'static str),
    /// Its local devices, as (count, GB each).
    instance_store: (i64, i64),
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its processor's sustained frequency, in GHz.
    sustained_ghz: f64,
    /// The core counts it can be configured with: (smallest, step) of its published list,
    /// which runs up to its default core count.
    valid_cores: (i64, i64),
}

/// Every size of the C5d family, with the figures AWS publishes for each.
///
/// One family of ONE hardware design, so nothing but the published row separates two rows:
/// the EBS figures burst to the family's dedicated row on the three smallest sizes and are
/// dedicated from the `4xlarge` up, two sizes sharing each figure above it; the local store is
/// one device that doubles with the size up to 900 GB and then two and four devices of that
/// size; the two sizes built on the Cascade Lake processor sustain 3.6 GHz where the rest
/// sustain 3.4; and the volumes each virtualized size holds are what the shared Nitro
/// attachment pool leaves once its devices are counted, while the bare metal size publishes
/// the 31 of a bare metal instance, whose local devices are the host's own hardware.
const ROWS: [Row; 9] = [
    Row {
        instance_type: "c5d.large",
        ebs: (650, 4_000, 4_750, 20_000),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 50),
        network_performance: "Up to 10 Gigabit",
        sustained_ghz: 3.4,
        valid_cores: (1, 1),
    },
    Row {
        instance_type: "c5d.xlarge",
        ebs: (1_150, 6_000, 4_750, 20_000),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 100),
        network_performance: "Up to 10 Gigabit",
        sustained_ghz: 3.4,
        valid_cores: (2, 2),
    },
    Row {
        instance_type: "c5d.2xlarge",
        ebs: (2_300, 10_000, 4_750, 20_000),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 200),
        network_performance: "Up to 10 Gigabit",
        sustained_ghz: 3.4,
        valid_cores: (2, 2),
    },
    Row {
        instance_type: "c5d.4xlarge",
        ebs: (4_750, 20_000, 4_750, 20_000),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 400),
        network_performance: "Up to 10 Gigabit",
        sustained_ghz: 3.4,
        valid_cores: (2, 2),
    },
    Row {
        instance_type: "c5d.9xlarge",
        ebs: (9_500, 40_000, 9_500, 40_000),
        ebs_attachments: (26, "shared"),
        instance_store: (1, 900),
        network_performance: "12 Gigabit",
        sustained_ghz: 3.4,
        valid_cores: (2, 2),
    },
    Row {
        instance_type: "c5d.12xlarge",
        ebs: (9_500, 40_000, 9_500, 40_000),
        ebs_attachments: (25, "shared"),
        instance_store: (2, 900),
        network_performance: "12 Gigabit",
        sustained_ghz: 3.6,
        valid_cores: (2, 2),
    },
    Row {
        instance_type: "c5d.18xlarge",
        ebs: (19_000, 80_000, 19_000, 80_000),
        ebs_attachments: (25, "shared"),
        instance_store: (2, 900),
        network_performance: "25 Gigabit",
        sustained_ghz: 3.4,
        // The published list of this size starts at four cores rather than at its step.
        valid_cores: (4, 2),
    },
    Row {
        instance_type: "c5d.24xlarge",
        ebs: (19_000, 80_000, 19_000, 80_000),
        ebs_attachments: (23, "shared"),
        instance_store: (4, 900),
        network_performance: "25 Gigabit",
        sustained_ghz: 3.6,
        valid_cores: (2, 2),
    },
    Row {
        instance_type: "c5d.metal",
        ebs: (19_000, 80_000, 19_000, 80_000),
        ebs_attachments: (31, "shared"),
        instance_store: (4, 900),
        network_performance: "25 Gigabit",
        sustained_ghz: 3.6,
        valid_cores: (1, 1),
    },
];

/// The core counts the published list of `row` holds: its step from its smallest count up to
/// the default cores a type of `vcpus` vCPUs runs.
fn published_core_counts(row: &Row, default_cores: i64) -> Vec<i64> {
    let (minimum, step) = row.valid_cores;
    (1..=default_cores)
        .filter(|cores| cores % step == 0 && *cores >= minimum)
        .collect()
}

/// Rule: the EBS, instance store, network and processor members a type reports are the figures
/// AWS publishes for THAT type.
#[tokio::test]
async fn published_rows_report_the_hardware_facts_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in ROWS {
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

        let storage = required(case, &body, "instanceStorageInfo");
        let (devices, gb_each) = row.instance_store;
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

/// Rule: the core counts a type can be configured with are the ones AWS publishes for it —
/// its own step from its own smallest count, not every count up to its default.
#[tokio::test]
async fn published_rows_report_the_core_counts_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let v_cpu_info = required(case, &body, "vCpuInfo");
        let default_cores = number(case, &v_cpu_info, "defaultCores") as i64;
        let expected = published_core_counts(&row, default_cores);
        let valid_cores = required(case, &v_cpu_info, "validCores");
        let reported: Vec<i64> = elements(&valid_cores, "item")
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

/// Rule: the host features a type is offered are published per type — no size of a family
/// whose local devices a migration cannot carry is offered reboot migration, and the bare
/// metal size's Dedicated Host, boot mode and Traffic Mirroring answers are its own.
#[tokio::test]
async fn published_rows_report_the_host_features_of_the_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: one claim per member, read on every size of the ladder for the two the whole
    // family publishes and on the bare metal size beside a virtualized one for the rest.
    let mut feature_rows: Vec<(&str, &str, &str)> = Vec::new();
    for row in ROWS {
        feature_rows.push((row.instance_type, "rebootMigrationSupport", "unsupported"));
        feature_rows.push((row.instance_type, "phcSupport", "unsupported"));
    }
    feature_rows.extend([
        ("c5d.large", "dedicatedHostsSupported", "true"),
        ("c5d.24xlarge", "dedicatedHostsSupported", "true"),
        ("c5d.metal", "dedicatedHostsSupported", "false"),
        ("c5d.xlarge", "trafficMirroringSupport", "supported"),
        ("c5d.metal", "trafficMirroringSupport", "supported"),
    ]);

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        // A member of networkInfo is read out of that block; the rest are the type's own.
        let scope = if member == "trafficMirroringSupport" {
            required(&case, &body, "networkInfo")
        } else {
            body
        };
        let reported = required(&case, &scope, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The boot modes of the bare metal size are its own: it is offered the legacy BIOS mode
    // alone where the virtualized sizes of the same family are offered both.
    for (instance_type, expected) in [
        ("c5d.metal", vec!["legacy-bios"]),
        ("c5d.24xlarge", vec!["legacy-bios", "uefi"]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let modes = required(case, &body, "supportedBootModes");
        let reported = elements(&modes, "item");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, as an invariant over every row: a type sustains no more than it reaches, and the
/// published figures do not follow the size multiplier — two sizes of the family share one
/// dedicated figure where the multiplier differs, and the family disagrees at one size with
/// the family whose name differs by a single capability letter.
#[tokio::test]
async fn the_published_figures_are_bounded_and_do_not_follow_the_size_multiplier() {
    let (router, _engine) = super::ec2_query::emulator();

    let bandwidth = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumBandwidthInMbps") as i64,
        )
    };

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let (sustained, reached) = bandwidth(&body, case);
        assert!(
            sustained > 0 && sustained <= reached,
            "case {case:?}: a type sustains at most what it reaches, got {sustained}/{reached}"
        );
    }

    // Two sizes of the family, a third of the ladder apart, carrying the SAME dedicated
    // figure: the multiplier cannot be what produced either.
    let left = described("c5d.18xlarge", &router, "c5d.18xlarge").await;
    let right = described("c5d.24xlarge", &router, "c5d.24xlarge").await;
    assert_eq!(
        bandwidth(&left, "c5d.18xlarge"),
        bandwidth(&right, "c5d.24xlarge"),
        "the two largest virtualized sizes share one published EBS figure"
    );

    // One size, two families that differ by a capability letter: the local devices of the one
    // that carries them are published for it, and the other carries none at all.
    let case = "c5.9xlarge";
    let plain = described(case, &router, case).await;
    assert!(
        element(&plain, "instanceStorageInfo").is_none(),
        "case {case:?}: a type with no local devices reports no instanceStorageInfo, got \
         {plain}"
    );
    let case = "c5d.9xlarge";
    let local = described(case, &router, case).await;
    let storage = required(case, &local, "instanceStorageInfo");
    let reported = number(case, &storage, "totalSizeInGB") as i64;
    assert_eq!(
        reported, 900,
        "case {case:?}: expected totalSizeInGB 900, got {reported}"
    );
}
