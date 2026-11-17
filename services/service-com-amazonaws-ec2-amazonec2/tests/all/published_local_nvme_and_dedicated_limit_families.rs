// SPDX-License-Identifier: CC0-1.0

//! The published hardware of two families the catalog had no rows for, driven over the wire
//! this service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with
//! XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: every member `DescribeInstanceTypes` reports about an instance type's
//! HARDWARE is the figure AWS publishes for that ONE type, read from the type's own row of
//! the catalog, and no function of the size multiplier in the type's name stands in for it. A
//! type the catalog holds no row for keeps the figures the derivation computes, which is the
//! other half of the same rule and is a case here too.
//!
//! The tables span what the rule generalises over rather than one report's types: a family
//! whose local instance store grows by DEVICE SIZE at eight sizes and by device COUNT only at
//! the top three, while its EBS and network ladder is its no-local-store sibling's; that
//! sibling itself, which the reports behind this suite never named; a family whose EBS volumes
//! have an attachment limit of their own, whose network and EBS bandwidth is configurable and
//! whose connection tracking uses a far shorter idle timeout; a second, differently built
//! family that publishes the same attachment limit and timeout but no bandwidth weighting at
//! all; and a type with no row.

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

/// The four `EbsOptimizedInfo` bandwidth and IOPS figures of one size, and the two throughput
/// readings that are the bandwidth ones in the other unit.
fn assert_ebs(case: &str, xml: &str, ebs: (f64, f64, f64, f64)) {
    let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = ebs;
    for (member, expected) in [
        ("baselineBandwidthInMbps", baseline_mbps),
        ("baselineIops", baseline_iops),
        ("maximumBandwidthInMbps", maximum_mbps),
        ("maximumIops", maximum_iops),
        ("baselineThroughputInMBps", baseline_mbps / MEGABITS_PER_MEGABYTE),
        ("maximumThroughputInMBps", maximum_mbps / MEGABITS_PER_MEGABYTE),
    ] {
        let actual = number(case, xml, member);
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "case {case:?}: expected <{member}> {expected}, got {actual}"
        );
    }
}

/// The published hardware of one size of a family with a local instance store.
#[derive(Debug, Clone, Copy)]
struct LocalNvmeRow {
    /// `c7gd.12xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (f64, f64, f64, f64),
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// Its local devices as (count, GB each, GB in total), or `None` where it carries none.
    instance_store: Option<(i64, i64, i64)>,
    /// How many Amazon EBS volumes it can hold attached, out of the total it shares with its
    /// interfaces and its instance store devices.
    maximum_ebs_attachments: i64,
}

/// Rule: a family with a local instance store serves its published device size and count, and
/// the EBS and network ladder of the host it shares with its no-local-store sibling, from its
/// own rows — not from the size in each type's name.
///
/// The rows are the smallest size, the one above it, a middle size, the size at which the
/// family's bandwidth stops bursting, the first size whose device COUNT grows, the largest
/// virtualized size and the bare metal size, plus a size of the sibling family the reports
/// behind this suite never named, which carries the same EBS row and no local store at all.
#[tokio::test]
async fn local_nvme_family_rows_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    let rows = [
        LocalNvmeRow {
            instance_type: "c7gd.medium",
            ebs: (315.0, 2_500.0, 10_000.0, 40_000.0),
            network_performance: "Up to 12.5 Gigabit",
            instance_store: Some((1, 59, 59)),
            maximum_ebs_attachments: 26,
        },
        LocalNvmeRow {
            instance_type: "c7gd.large",
            ebs: (630.0, 3_600.0, 10_000.0, 40_000.0),
            network_performance: "Up to 12.5 Gigabit",
            instance_store: Some((1, 118, 118)),
            maximum_ebs_attachments: 26,
        },
        LocalNvmeRow {
            instance_type: "c7gd.2xlarge",
            ebs: (2_500.0, 12_000.0, 10_000.0, 40_000.0),
            network_performance: "Up to 15 Gigabit",
            instance_store: Some((1, 475, 475)),
            maximum_ebs_attachments: 26,
        },
        LocalNvmeRow {
            instance_type: "c7gd.8xlarge",
            ebs: (10_000.0, 40_000.0, 10_000.0, 40_000.0),
            network_performance: "15 Gigabit",
            instance_store: Some((1, 1_900, 1_900)),
            maximum_ebs_attachments: 26,
        },
        LocalNvmeRow {
            instance_type: "c7gd.12xlarge",
            ebs: (15_000.0, 60_000.0, 15_000.0, 60_000.0),
            network_performance: "22.5 Gigabit",
            instance_store: Some((2, 1_425, 2_850)),
            maximum_ebs_attachments: 25,
        },
        LocalNvmeRow {
            instance_type: "c7gd.16xlarge",
            ebs: (20_000.0, 80_000.0, 20_000.0, 80_000.0),
            network_performance: "30 Gigabit",
            instance_store: Some((2, 1_900, 3_800)),
            maximum_ebs_attachments: 25,
        },
        LocalNvmeRow {
            instance_type: "c7gd.metal",
            ebs: (20_000.0, 80_000.0, 20_000.0, 80_000.0),
            network_performance: "30 Gigabit",
            instance_store: Some((2, 1_900, 3_800)),
            maximum_ebs_attachments: 31,
        },
        LocalNvmeRow {
            instance_type: "c7g.4xlarge",
            ebs: (5_000.0, 20_000.0, 10_000.0, 40_000.0),
            network_performance: "Up to 15 Gigabit",
            instance_store: None,
            maximum_ebs_attachments: 27,
        },
    ];

    for row in rows {
        let case = row.instance_type;
        let xml = described(case, &router, row.instance_type).await;

        assert_ebs(case, &xml, row.ebs);

        let attachments = number(case, &xml, "maximumEbsAttachments");
        let expected_attachments = row.maximum_ebs_attachments as f64;
        assert!(
            (attachments - expected_attachments).abs() < f64::EPSILON,
            "case {case:?}: expected <maximumEbsAttachments> {expected_attachments}, \
             got {attachments}"
        );

        let limit_kind = required(case, &xml, "attachmentLimitType");
        assert_eq!(
            limit_kind, "shared",
            "case {case:?}: expected a shared attachment limit, got {limit_kind:?}"
        );

        let performance = required(case, &xml, "networkPerformance");
        assert_eq!(
            performance, row.network_performance,
            "case {case:?}: expected <networkPerformance> {:?}, got {performance:?}",
            row.network_performance
        );

        let ghz = number(case, &xml, "sustainedClockSpeedInGhz");
        assert!(
            (ghz - 2.6).abs() < f64::EPSILON,
            "case {case:?}: expected <sustainedClockSpeedInGhz> 2.6, got {ghz}"
        );

        match row.instance_store {
            None => {
                let storage = element(&xml, "instanceStorageInfo");
                assert!(
                    storage.is_none(),
                    "case {case:?}: expected no <instanceStorageInfo>, got {storage:?}"
                );
            }
            Some((count, gb_each, total)) => {
                let storage = required(case, &xml, "instanceStorageInfo");
                let reported_total = number(case, &storage, "totalSizeInGB");
                assert!(
                    (reported_total - total as f64).abs() < f64::EPSILON,
                    "case {case:?}: expected <totalSizeInGB> {total}, got {reported_total}"
                );
                let disks = elements(&required(case, &storage, "disks"), "item");
                assert_eq!(
                    disks.len(),
                    1,
                    "case {case:?}: expected one device entry, got {disks:?}"
                );
                let reported_size = number(case, &disks[0], "sizeInGB");
                assert!(
                    (reported_size - gb_each as f64).abs() < f64::EPSILON,
                    "case {case:?}: expected <sizeInGB> {gb_each}, got {reported_size}"
                );
                let reported_count = number(case, &disks[0], "count");
                assert!(
                    (reported_count - count as f64).abs() < f64::EPSILON,
                    "case {case:?}: expected <count> {count}, got {reported_count}"
                );
            }
        }
    }
}

/// The published facts of one size of a family whose EBS volumes have a limit of their own.
#[derive(Debug, Clone, Copy)]
struct DedicatedLimitRow {
    /// `r9g.24xlarge` and the like.
    instance_type: &'static str,
    /// How many Amazon EBS volumes it can hold attached, that limit being the volumes' own,
    /// where the catalog records the published figure for the size.
    maximum_ebs_attachments: Option<i64>,
    /// The bandwidth weightings a launch may ask for, empty where its bandwidth split is
    /// fixed and it publishes none.
    bandwidth_weightings: &'static [&'static str],
    /// The idle timeout, in seconds, its connection tracking applies to an established TCP
    /// connection by default.
    tcp_established_timeout: i64,
    /// The Elastic Fabric Adapters it is offered, where the catalog records the figure.
    efa_interfaces: Option<i64>,
}

/// Rule: the same per-type rows answer the attachment limit KIND, the bandwidth weightings, the
/// connection tracking default and the adapter counts — so two families built differently agree
/// on the figures they share and differ on the ones they do not.
///
/// The rows are five sizes of a memory optimized family on the newest hardware generation,
/// including both sizes that hold a whole host, and one size of a differently built family the
/// reports behind this suite never named, which publishes the same attachment limit and the same
/// tracking default while publishing no bandwidth weighting at all.
#[tokio::test]
async fn dedicated_attachment_limit_rows_are_served_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    /// The weightings a family whose network and EBS bandwidth can be weighted publishes.
    const CONFIGURABLE: &[&str] = &["default", "ebs-1", "vpc-1"];

    let rows = [
        DedicatedLimitRow {
            instance_type: "r9g.medium",
            maximum_ebs_attachments: Some(32),
            bandwidth_weightings: CONFIGURABLE,
            tcp_established_timeout: 350,
            efa_interfaces: None,
        },
        DedicatedLimitRow {
            instance_type: "r9g.4xlarge",
            maximum_ebs_attachments: Some(32),
            bandwidth_weightings: CONFIGURABLE,
            tcp_established_timeout: 350,
            efa_interfaces: None,
        },
        DedicatedLimitRow {
            instance_type: "r9g.12xlarge",
            maximum_ebs_attachments: Some(32),
            bandwidth_weightings: CONFIGURABLE,
            tcp_established_timeout: 350,
            efa_interfaces: None,
        },
        DedicatedLimitRow {
            instance_type: "r9g.48xlarge",
            maximum_ebs_attachments: None,
            bandwidth_weightings: CONFIGURABLE,
            tcp_established_timeout: 350,
            efa_interfaces: Some(16),
        },
        DedicatedLimitRow {
            instance_type: "r9g.metal-48xl",
            maximum_ebs_attachments: None,
            bandwidth_weightings: CONFIGURABLE,
            tcp_established_timeout: 350,
            efa_interfaces: Some(16),
        },
        DedicatedLimitRow {
            instance_type: "r8ib.large",
            maximum_ebs_attachments: Some(32),
            bandwidth_weightings: &[],
            tcp_established_timeout: 350,
            efa_interfaces: None,
        },
    ];

    for row in rows {
        let case = row.instance_type;
        let xml = described(case, &router, row.instance_type).await;

        let limit_kind = required(case, &xml, "attachmentLimitType");
        assert_eq!(
            limit_kind, "dedicated",
            "case {case:?}: expected a dedicated attachment limit, got {limit_kind:?}"
        );

        // The published figure is asserted only where the catalog records it for the size; a
        // size whose row carries the KIND of the limit without a figure keeps the derived
        // count, and the KIND above is what that row pins.
        if let Some(expected) = row.maximum_ebs_attachments {
            let attachments = number(case, &xml, "maximumEbsAttachments");
            let expected_attachments = expected as f64;
            assert!(
                (attachments - expected_attachments).abs() < f64::EPSILON,
                "case {case:?}: expected <maximumEbsAttachments> {expected_attachments}, \
                 got {attachments}"
            );
        }

        let weightings = element(&xml, "bandwidthWeightings")
            .map(|list| elements(&list, "item"))
            .unwrap_or_default();
        assert_eq!(
            weightings, row.bandwidth_weightings,
            "case {case:?}: expected <bandwidthWeightings> {:?}, got {weightings:?}",
            row.bandwidth_weightings
        );

        let timeout = number(case, &xml, "defaultTcpEstablishedTimeout");
        let expected_timeout = row.tcp_established_timeout as f64;
        assert!(
            (timeout - expected_timeout).abs() < f64::EPSILON,
            "case {case:?}: expected <defaultTcpEstablishedTimeout> {expected_timeout}, \
             got {timeout}"
        );

        let mirroring = required(case, &xml, "trafficMirroringSupport");
        assert_eq!(
            mirroring, "unsupported",
            "case {case:?}: expected <trafficMirroringSupport> \"unsupported\", got {mirroring:?}"
        );

        let secondary = required(case, &xml, "secondaryNetworkSupported");
        assert_eq!(
            secondary, "false",
            "case {case:?}: expected <secondaryNetworkSupported> \"false\", got {secondary:?}"
        );

        let efa_supported = required(case, &xml, "efaSupported");
        match row.efa_interfaces {
            None => assert_eq!(
                efa_supported, "false",
                "case {case:?}: expected <efaSupported> \"false\", got {efa_supported:?}"
            ),
            Some(adapters) => {
                assert_eq!(
                    efa_supported, "true",
                    "case {case:?}: expected <efaSupported> \"true\", got {efa_supported:?}"
                );
                let reported = number(case, &required(case, &xml, "efaInfo"), "maximumEfaInterfaces");
                assert!(
                    (reported - adapters as f64).abs() < f64::EPSILON,
                    "case {case:?}: expected <maximumEfaInterfaces> {adapters}, got {reported}"
                );
            }
        }
    }
}

/// Rule, the other half: a type the catalog holds NO row for is described by the derivation
/// alone — it shares one attachment total with its other devices, publishes no bandwidth
/// weighting and keeps the long established-TCP idle timeout the earlier generations report.
#[tokio::test]
async fn a_type_with_no_published_row_keeps_the_derived_figures() {
    let (router, _engine) = super::ec2_query::emulator();

    let case = "m5.large";
    let xml = described(case, &router, case).await;

    let limit_kind = required(case, &xml, "attachmentLimitType");
    assert_eq!(
        limit_kind, "shared",
        "case {case:?}: expected a shared attachment limit, got {limit_kind:?}"
    );

    let attachments = number(case, &xml, "maximumEbsAttachments");
    assert!(
        (attachments - 27.0).abs() < f64::EPSILON,
        "case {case:?}: expected <maximumEbsAttachments> 27, got {attachments}"
    );

    let weightings = element(&xml, "bandwidthWeightings");
    assert!(
        weightings.is_none(),
        "case {case:?}: expected no <bandwidthWeightings>, got {weightings:?}"
    );

    let timeout = number(case, &xml, "defaultTcpEstablishedTimeout");
    assert!(
        (timeout - 432_000.0).abs() < f64::EPSILON,
        "case {case:?}: expected <defaultTcpEstablishedTimeout> 432000, got {timeout}"
    );
}
