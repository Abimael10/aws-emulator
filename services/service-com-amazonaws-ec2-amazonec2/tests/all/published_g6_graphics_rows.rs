// SPDX-License-Identifier: CC0-1.0

//! The per-instance-type rows the catalog publishes for the sixth generation x86 graphics
//! family, driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: EVERY MEMBER `DescribeInstanceTypes` REPORTS ABOUT AN INSTANCE TYPE IS
//! THE FACT AWS PUBLISHES FOR THAT ONE TYPE, read off a per-type table — never a function of
//! the family letter and the size multiplier in its name. An accelerated type therefore
//! describes its accelerators (`InstanceTypeInfo$GpuInfo`), the EBS bandwidth and IOPS it
//! sustains and the figures it reaches (`EbsInfo$EbsOptimizedInfo`), how many volumes it holds
//! attached and whether that limit is the volumes' own (`$MaximumEbsAttachments` /
//! `$AttachmentLimitType`), its local devices (`InstanceStorageInfo`), its network column
//! (`NetworkInfo$NetworkPerformance`), its processor and that processor's sustained frequency
//! (`ProcessorInfo`), the adapters it is offered (`$EfaSupported`/`$EfaInfo`,
//! `$EnaSrdSupported`) and the host features it is offered (`$DedicatedHostsSupported`,
//! `$RebootMigrationSupport`, `$PhcSupport`) as figures of its own.
//!
//! The table below spans what the rule generalises over: every size of one family, which differ
//! in accelerator count (one on five of them, four on two and eight on the largest), in local
//! device count and size, in whether their EBS and network shares are dedicated or burst to a
//! ceiling, in their volume-attachment limit (four distinct figures over the ladder), and in
//! being offered an Elastic Fabric Adapter or ENA Express at all — so no one size reads as the
//! specification. A non-accelerated type of the same hardware generation is a row too, to pin
//! that the accelerator inventory is a fact of the type rather than of the catalog.
//!
//! The figures are the ones AWS states for each type: the family's own instance-type
//! specification page (accelerators, vCPUs, memory, local devices, network performance), the
//! "Amazon EBS optimized instance types" table (the EBS figures), "Instance volume limits" (the
//! attachment limit and its kind) and the published lists of the types offered an Elastic
//! Fabric Adapter, ENA Express and Dedicated Hosts. Three figures of the table are DERIVED
//! rather than read off those tables, and say so where they sit: the volume limits of the
//! `16xlarge`, `24xlarge` and `48xlarge` (the family's limit-per-host-share ladder, the
//! half-host rung of it confirmed by the sibling `g6e` family) and the sustained EBS figures of
//! the two sizes below the family's dedicated row (the family's own rate per vCPU).

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    assert_published_ebs, described_instance_type, emulator, reported_instance_type_member,
};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-west-2";

/// The MiB in a GiB, for reading a published memory figure stated in GiB.
const MIB_PER_GIB: i64 = 1_024;

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    described_instance_type(case, router, REGION, instance_type).await
}

/// One member of the reported type, or a panic naming the case when the type reports none.
fn required(case: &str, body: &str, path: &[&str]) -> String {
    reported_instance_type_member(case, body, path)
        .unwrap_or_else(|| panic!("case {case:?}: no {path:?} reported, got {body}"))
}

/// One member of the reported type as a number.
fn number(case: &str, body: &str, path: &[&str]) -> f64 {
    let text = required(case, body, path);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {path:?} must be a number, got {text:?}"))
}

/// The published row of one instance type: the figures a client sizes its accelerators, its
/// storage, its volumes and its network from.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// `g6.12xlarge` and the like.
    instance_type: &'static str,
    /// Its memory, in GiB as its specification page states it.
    memory_gib: i64,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// How many Amazon EBS volumes it holds attached, which for this family is a limit of the
    /// volumes' own rather than one shared with its interfaces and local devices. DERIVED from
    /// the family's ladder on the three largest sizes (see the module header).
    ebs_attachments: i64,
    /// Its local devices, as (count, GB each).
    instance_store: (i64, i64),
    /// "The network performance", as the published table writes that column.
    network_performance: &'static str,
    /// Its accelerators, as (count, the device's name, the MiB of ONE device), or `None` for a
    /// type that carries none at all.
    accelerators: Option<(i64, &'static str, i64)>,
    /// The Elastic Fabric Adapters it is offered, zero where it is offered none.
    efa_interfaces: i64,
    /// Is ENA Express offered on it?
    ena_srd: bool,
}

/// Every size of the sixth generation x86 graphics family, with the figures AWS publishes for
/// each.
///
/// The two sizes below the family's dedicated EBS row burst above the share they sustain, and
/// the sustained figure each publishes is its share at the family's own rate — 500 Mbps and the
/// four IOPS per Mbps every recorded row of the family holds — rather than a figure of the
/// generic size ladder.
const ROWS: &[Row] = &[
    Row {
        instance_type: "g6.xlarge",
        memory_gib: 16,
        ebs: (2_000, 8_000, 5_000, 20_000),
        ebs_attachments: 32,
        instance_store: (1, 250),
        network_performance: "Up to 10 Gigabit",
        accelerators: Some((1, "L4", 22_888)),
        efa_interfaces: 0,
        ena_srd: false,
    },
    Row {
        instance_type: "g6.2xlarge",
        memory_gib: 32,
        ebs: (4_000, 16_000, 5_000, 20_000),
        ebs_attachments: 32,
        instance_store: (1, 450),
        network_performance: "Up to 10 Gigabit",
        accelerators: Some((1, "L4", 22_888)),
        efa_interfaces: 0,
        ena_srd: false,
    },
    Row {
        instance_type: "g6.4xlarge",
        memory_gib: 64,
        ebs: (8_000, 32_000, 8_000, 32_000),
        ebs_attachments: 32,
        instance_store: (1, 600),
        network_performance: "Up to 25 Gigabit",
        accelerators: Some((1, "L4", 22_888)),
        efa_interfaces: 0,
        ena_srd: false,
    },
    Row {
        instance_type: "g6.8xlarge",
        memory_gib: 128,
        ebs: (16_000, 64_000, 16_000, 64_000),
        ebs_attachments: 32,
        instance_store: (2, 450),
        network_performance: "25 Gigabit",
        accelerators: Some((1, "L4", 22_888)),
        efa_interfaces: 1,
        ena_srd: false,
    },
    Row {
        instance_type: "g6.12xlarge",
        memory_gib: 192,
        ebs: (20_000, 80_000, 20_000, 80_000),
        ebs_attachments: 32,
        instance_store: (4, 940),
        network_performance: "40 Gigabit",
        accelerators: Some((4, "L4", 22_888)),
        efa_interfaces: 1,
        ena_srd: false,
    },
    Row {
        instance_type: "g6.16xlarge",
        memory_gib: 256,
        ebs: (20_000, 80_000, 20_000, 80_000),
        ebs_attachments: 48,
        instance_store: (2, 940),
        network_performance: "25 Gigabit",
        accelerators: Some((1, "L4", 22_888)),
        efa_interfaces: 1,
        ena_srd: false,
    },
    Row {
        instance_type: "g6.24xlarge",
        memory_gib: 384,
        ebs: (30_000, 120_000, 30_000, 120_000),
        ebs_attachments: 64,
        instance_store: (4, 940),
        network_performance: "50 Gigabit",
        accelerators: Some((4, "L4", 22_888)),
        efa_interfaces: 1,
        ena_srd: false,
    },
    Row {
        instance_type: "g6.48xlarge",
        memory_gib: 768,
        ebs: (60_000, 240_000, 60_000, 240_000),
        ebs_attachments: 128,
        instance_store: (8, 940),
        network_performance: "100 Gigabit",
        accelerators: Some((8, "L4", 22_888)),
        efa_interfaces: 1,
        ena_srd: true,
    },
];

/// Rule, row by row: every member is the figure AWS publishes for THAT instance type.
#[tokio::test]
async fn every_published_graphics_row_is_reported_for_its_own_instance_type() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;

        let reported = number(case, &body, &["memoryInfo", "sizeInMiB"]) as i64;
        let expected = row.memory_gib * MIB_PER_GIB;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected memoryInfo.sizeInMiB {expected}, got {reported}"
        );

        let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = row.ebs;
        // The six readings of one published row, through the shared reader so the Mbps and the
        // MB/s of one figure cannot be asserted inconsistently.
        assert_published_ebs(
            case,
            &body,
            (
                baseline_mbps as f64,
                baseline_iops as f64,
                maximum_mbps as f64,
                maximum_iops as f64,
            ),
        );

        let reported = number(case, &body, &["ebsInfo", "maximumEbsAttachments"]) as i64;
        assert_eq!(
            reported, row.ebs_attachments,
            "case {case:?}: expected maximumEbsAttachments {}, got {reported}",
            row.ebs_attachments
        );
        // The family's volumes are attached to EBS hardware of the type's own, so the limit is
        // the volumes' own rather than shared with its interfaces and local devices.
        let reported = required(case, &body, &["ebsInfo", "attachmentLimitType"]);
        assert_eq!(
            reported, "dedicated",
            "case {case:?}: expected a dedicated attachment limit, got {reported:?}"
        );

        let (devices, gb_each) = row.instance_store;
        let reported = number(case, &body, &["instanceStorageInfo", "disks", "item", "count"])
            as i64;
        assert_eq!(
            reported, devices,
            "case {case:?}: expected {devices} local devices, got {reported}"
        );
        let reported =
            number(case, &body, &["instanceStorageInfo", "disks", "item", "sizeInGB"]) as i64;
        assert_eq!(
            reported, gb_each,
            "case {case:?}: expected local devices of {gb_each} GB, got {reported}"
        );
        // The total is one device's size times the count, so the two readings of one instance
        // store cannot disagree.
        let expected = devices * gb_each;
        let reported = number(case, &body, &["instanceStorageInfo", "totalSizeInGB"]) as i64;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected totalSizeInGB {expected}, got {reported}"
        );

        let reported = required(case, &body, &["networkInfo", "networkPerformance"]);
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );

        // The processor of the whole family, and the frequency it sustains.
        let reported = required(case, &body, &["processorInfo", "manufacturer"]);
        assert_eq!(
            reported, "AMD",
            "case {case:?}: expected the family's processor maker, got {reported:?}"
        );
        let reported = number(case, &body, &["processorInfo", "sustainedClockSpeedInGhz"]);
        assert!(
            (reported - 3.4).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz 3.4, got {reported}"
        );

        match row.accelerators {
            Some((count, name, memory_mib_each)) => {
                let reported = number(case, &body, &["gpuInfo", "gpus", "item", "count"]) as i64;
                assert_eq!(
                    reported, count,
                    "case {case:?}: expected {count} accelerators, got {reported}"
                );
                let reported = required(case, &body, &["gpuInfo", "gpus", "item", "name"]);
                assert_eq!(
                    reported, name,
                    "case {case:?}: expected the accelerator {name:?}, got {reported:?}"
                );
                let reported = required(case, &body, &["gpuInfo", "gpus", "item", "manufacturer"]);
                assert_eq!(
                    reported, "NVIDIA",
                    "case {case:?}: expected the accelerator's maker, got {reported:?}"
                );
                let reported = number(
                    case,
                    &body,
                    &["gpuInfo", "gpus", "item", "memoryInfo", "sizeInMiB"],
                ) as i64;
                assert_eq!(
                    reported, memory_mib_each,
                    "case {case:?}: expected {memory_mib_each} MiB per accelerator, got {reported}"
                );
                let expected = count * memory_mib_each;
                let reported = number(case, &body, &["gpuInfo", "totalGpuMemoryInMiB"]) as i64;
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected totalGpuMemoryInMiB {expected}, got {reported}"
                );
            }
            None => {
                let reported =
                    reported_instance_type_member(case, &body, &["gpuInfo", "totalGpuMemoryInMiB"]);
                assert!(
                    reported.is_none(),
                    "case {case:?}: a type with no accelerator describes none, got {reported:?}"
                );
            }
        }

        let reported = required(case, &body, &["networkInfo", "efaSupported"]);
        let expected = if row.efa_interfaces > 0 { "true" } else { "false" };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected efaSupported {expected:?}, got {reported:?}"
        );
        let adapters =
            reported_instance_type_member(case, &body, &["networkInfo", "efaInfo", "maximumEfaInterfaces"]);
        if row.efa_interfaces > 0 {
            let reported = adapters
                .unwrap_or_else(|| panic!("case {case:?}: no efaInfo reported, got {body}"))
                .parse::<i64>()
                .unwrap_or_else(|_| panic!("case {case:?}: an adapter count, got {body}"));
            assert_eq!(
                reported, row.efa_interfaces,
                "case {case:?}: expected {} Elastic Fabric Adapters, got {reported}",
                row.efa_interfaces
            );
        } else {
            assert!(
                adapters.is_none(),
                "case {case:?}: a type offered no Elastic Fabric Adapter describes none, \
                 got {adapters:?}"
            );
        }

        let reported = required(case, &body, &["networkInfo", "enaSrdSupported"]);
        let expected = if row.ena_srd { "true" } else { "false" };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected enaSrdSupported {expected:?}, got {reported:?}"
        );

        // The host features the whole family publishes: it is offered on no Dedicated Host,
        // no size of it is migrated to another host on a user-initiated reboot, and none
        // carries a local Precision Time Protocol hardware clock.
        for (path, expected) in [
            (vec!["dedicatedHostsSupported"], "false"),
            (vec!["rebootMigrationSupport"], "unsupported"),
            (vec!["phcSupport"], "unsupported"),
            (vec!["networkInfo", "secondaryNetworkSupported"], "false"),
            (vec!["networkInfo", "ipv4AddressesPerSecondaryInterface"], "0"),
        ] {
            let reported = required(case, &body, &path);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {path:?} {expected:?}, got {reported:?}"
            );
        }
    }
}

/// Rule: the accelerator inventory is a fact of the TYPE — a type of the same hardware
/// generation that carries no accelerator describes none at all.
#[tokio::test]
async fn a_type_with_no_accelerator_of_the_same_generation_describes_none() {
    let (router, _engine) = emulator();

    for case in ["m6i.4xlarge", "c6a.8xlarge", "r6i.xlarge"] {
        let body = described(case, &router, case).await;
        let reported =
            reported_instance_type_member(case, &body, &["gpuInfo", "totalGpuMemoryInMiB"]);
        assert!(
            reported.is_none(),
            "case {case:?}: a type with no accelerator describes no gpuInfo, got {reported:?}"
        );
    }
}
