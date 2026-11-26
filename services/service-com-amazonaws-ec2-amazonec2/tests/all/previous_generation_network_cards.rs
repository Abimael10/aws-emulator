// SPDX-License-Identifier: CC0-1.0

//! What `DescribeInstanceTypes` serves for the network adapter of a published instance type:
//! the two bandwidth figures of its card, the words of its "Network performance" column, and
//! the ENA queues one of its interfaces is given.
//!
//! The rules under test:
//!
//! 1. A type's network figures are the ones its own hardware publishes, not a share of a
//!    later generation's host: the card of a size reports the figure that size sustains — its
//!    own step of the family's published baseline ladder — and the whole figure of the one
//!    adapter its family's sizes share as the figure it can reach, at EVERY size, including a
//!    size that holds the whole host.
//! 2. A card's `networkPerformance` is the published column of the type it belongs to, which
//!    is the same text the type itself reports.
//! 3. `defaultEnaQueueCountPerInterface` is reported for every type: the published count of
//!    the adapter's queues on a type that HAS an Elastic Network Adapter, and the single queue
//!    the device presents on a type that predates it (`enaSupport` `unsupported`).
//!
//! The rows are read back through the operation end to end, one ec2Query call per type, the
//! way `ec2_query` drives every suite in this crate.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    described_instance_type, instance_type_member_number, reported_instance_type_member,
    required_instance_type_member,
};

/// The emulator the tests drive. This operation reports facts of a type and stores nothing,
/// so the engine handle it hands back is not needed here.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The Region every call here is signed for. The published hardware of an instance type is
/// the same in every Region, so one Region covers the rule.
const REGION: &str = "us-west-2";

/// The member path of one figure of the first (and, for these families, only) network card a
/// type reports.
fn card(member: &str) -> [&str; 4] {
    ["networkInfo", "networkCards", "item", member]
}

/// One row of the network table: the type, the sustained and peak Gigabit its card carries,
/// and the words of its "Network performance" column.
type NetworkCase<'a> = (&'a str, f64, f64, &'a str);

/// Rule 1 and rule 2 over the families whose sizes share ONE host adapter: the first
/// generation AWS Graviton family, which publishes `Up to 10 Gigabit` at every size, two
/// pre-Nitro families whose column carries words instead of a figure, and the two burstable
/// families whose network bursts the way their CPU does.
///
/// The rows span the class along what the rule generalises over: the smallest and the largest
/// size of a ladder (the bare metal size included), a family whose column reads in words at
/// one size and in Gigabit at another, a family whose sizes never reach the adapter's figure
/// and one whose largest sizes do, and three different adapter figures.
#[tokio::test]
async fn a_network_card_reports_its_own_hosts_figures() {
    let router = emulator();
    let cases: [NetworkCase; 17] = [
        // A1 — the published baseline ladder, below the host adapter's 10 Gigabit at every
        // size, which is why the column reads `Up to` even on the sizes that hold the host.
        ("a1.medium", 0.5, 10.0, "Up to 10 Gigabit"),
        ("a1.large", 0.75, 10.0, "Up to 10 Gigabit"),
        ("a1.xlarge", 1.25, 10.0, "Up to 10 Gigabit"),
        ("a1.2xlarge", 2.5, 10.0, "Up to 10 Gigabit"),
        ("a1.4xlarge", 5.0, 10.0, "Up to 10 Gigabit"),
        ("a1.metal", 5.0, 10.0, "Up to 10 Gigabit"),
        // C1 and C3 — pre-Nitro Xen hosts, whose column carries the published words and whose
        // card carries the size's step of the baseline ladder beside the adapter's own 10
        // Gigabit, of which one instance sustains at most five.
        ("c1.medium", 0.5, 10.0, "Moderate"),
        ("c1.xlarge", 1.25, 10.0, "High"),
        ("c3.large", 0.75, 10.0, "Moderate"),
        ("c3.xlarge", 1.25, 10.0, "Moderate"),
        ("c3.2xlarge", 2.5, 10.0, "High"),
        ("c3.4xlarge", 5.0, 10.0, "High"),
        ("c3.8xlarge", 5.0, 10.0, "10 Gigabit"),
        // The burstable families: one baseline ladder doubling from 32 Megabit, under the one
        // adapter figure each family's sizes share.
        ("t2.small", 0.128, 1.024, "Low to Moderate"),
        ("t2.2xlarge", 1.024, 1.024, "Moderate"),
        ("t4g.small", 0.128, 5.0, "Up to 5 Gigabit"),
        ("t4g.2xlarge", 2.048, 5.0, "Up to 5 Gigabit"),
    ];

    for (instance_type, baseline, peak, performance) in cases {
        let body = described_instance_type(instance_type, &router, REGION, instance_type).await;
        for (member, expected) in [
            ("baselineBandwidthInGbps", baseline),
            ("peakBandwidthInGbps", peak),
        ] {
            let actual = instance_type_member_number(instance_type, &body, &card(member));
            assert!(
                (actual - expected).abs() < 0.0001,
                "case {instance_type:?}: expected card {member} {expected}, got {actual}"
            );
        }
        let card_performance =
            required_instance_type_member(instance_type, &body, &card("networkPerformance"));
        assert_eq!(
            card_performance, performance,
            "case {instance_type:?}: expected card networkPerformance {performance:?}, \
             got {card_performance:?}"
        );
        // Rule 2: the card's words and the type's own are one published column.
        let reported = required_instance_type_member(
            instance_type,
            &body,
            &["networkInfo", "networkPerformance"],
        );
        assert_eq!(
            reported, performance,
            "case {instance_type:?}: expected networkPerformance {performance:?}, got {reported:?}"
        );
    }
}

/// Rule 3: the queues one interface is given, over the two kinds of adapter.
///
/// The rows differ along what the rule generalises over: a type whose row publishes its
/// adapter's queue count, a type with an adapter whose count this catalog does not hold, and
/// types from two different pre-ENA families and two different sizes, whose interfaces are not
/// ENA ones at all.
#[tokio::test]
async fn an_interface_reports_the_queues_its_adapter_gives_it() {
    let router = emulator();
    // (the type, its `enaSupport`, the queues one of its interfaces is given)
    let cases: [(&str, &str, Option<i32>); 8] = [
        // Pre-ENA Xen families: one queue per interface, whichever family and size.
        ("c1.medium", "unsupported", Some(1)),
        ("c1.xlarge", "unsupported", Some(1)),
        ("c3.large", "unsupported", Some(1)),
        ("c3.8xlarge", "unsupported", Some(1)),
        ("m3.xlarge", "unsupported", Some(1)),
        ("t2.small", "unsupported", Some(1)),
        // An ENA type whose published queue count the catalog holds, and one whose it does
        // not: the figure, and an absent member rather than an invented one.
        ("m5.large", "required", Some(2)),
        ("a1.large", "required", None),
    ];

    for (instance_type, ena_support, queues) in cases {
        let body = described_instance_type(instance_type, &router, REGION, instance_type).await;
        let reported_ena =
            required_instance_type_member(instance_type, &body, &["networkInfo", "enaSupport"]);
        assert_eq!(
            reported_ena, ena_support,
            "case {instance_type:?}: expected enaSupport {ena_support:?}, got {reported_ena:?}"
        );
        let reported = reported_instance_type_member(
            instance_type,
            &body,
            &card("defaultEnaQueueCountPerInterface"),
        );
        let expected = queues.map(|queues| queues.to_string());
        assert_eq!(
            reported, expected,
            "case {instance_type:?}: expected defaultEnaQueueCountPerInterface {expected:?}, \
             got {reported:?}"
        );
    }
}

/// One row of the specification table: the type, its memory in MiB, the devices and the GB
/// each of its instance store, its interfaces and its private addresses per interface.
type SpecificationCase<'a> = (&'a str, f64, (f64, f64), f64, f64);

/// The published rows of the two families whose specification figures this change states: the
/// memory, the local devices and the interface limits AWS publishes for them, none of which
/// the size multiplier in the name yields.
///
/// One row per size the family has, so the table spans the whole of both ladders rather than
/// the one size a report cited.
#[tokio::test]
async fn a_published_row_serves_the_specifications_figures() {
    let router = emulator();
    let cases: [SpecificationCase; 2] = [
        ("c1.medium", 1_740.0, (1.0, 350.0), 2.0, 6.0),
        ("c1.xlarge", 7_168.0, (4.0, 420.0), 4.0, 15.0),
    ];

    for (instance_type, memory_mib, (devices, gb_each), interfaces, addresses) in cases {
        let body = described_instance_type(instance_type, &router, REGION, instance_type).await;
        for (path, expected) in [
            (vec!["memoryInfo", "sizeInMiB"], memory_mib),
            (
                vec!["instanceStorageInfo", "disks", "item", "count"],
                devices,
            ),
            (
                vec!["instanceStorageInfo", "disks", "item", "sizeInGB"],
                gb_each,
            ),
            (
                vec!["instanceStorageInfo", "totalSizeInGB"],
                devices * gb_each,
            ),
            (vec!["networkInfo", "maximumNetworkInterfaces"], interfaces),
            (
                vec!["networkInfo", "ipv4AddressesPerInterface"],
                addresses,
            ),
        ] {
            let actual = instance_type_member_number(instance_type, &body, &path);
            assert!(
                (actual - expected).abs() < 0.0001,
                "case {instance_type:?}: expected {path:?} {expected}, got {actual}"
            );
        }
    }
}
