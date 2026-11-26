// SPDX-License-Identifier: CC0-1.0

//! The network CARD a type reports, driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: the bandwidth a type's network card sustains and the bandwidth it can
//! reach (`NetworkCardInfo$BaselineBandwidthInGbps` / `$PeakBandwidthInGbps`), the text of
//! those two figures (`$NetworkPerformance`) and the ENA queues each of its interfaces is
//! given by default (`$DefaultEnaQueueCountPerInterface`) are facts AWS publishes PER
//! INSTANCE TYPE — not the size's share of its host, and not one answer for a whole
//! hardware generation.
//!
//! The tables below span what that rule generalises over: four families on three different
//! hosts (a 25 Gigabit compute optimized ladder built on two processor generations, a
//! 20 Gigabit AMD one, and a 100 Gigabit network optimized one), and, within each, the
//! smallest size, the sizes that burst above their baseline, the sizes whose figure is
//! dedicated, the largest virtualized size and a bare metal size where the family has one.
//! The closing test holds two families at ONE size against each other and checks the text of
//! every row against the figures of that same row, so no single row can be read as the
//! specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{described_instance_type, reported_instance_type_member, required};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// How close two bandwidth figures must be to count as the same published figure.
const BANDWIDTH_TOLERANCE: f64 = 0.001;

/// The published network card of one instance type.
#[derive(Debug, Clone, Copy)]
struct PublishedCard {
    /// `c5.9xlarge` and the like.
    instance_type: &'static str,
    /// The bandwidth the card sustains, in Gbps.
    baseline_gbps: f64,
    /// The bandwidth it can reach, in Gbps.
    peak_gbps: f64,
    /// Those two figures as the published table writes the column.
    network_performance: &'static str,
    /// The ENA queues each interface attached to the card is given by default.
    ena_queues: i64,
}

/// Every row of the tables below, in one list so each test may walk the whole class.
fn published_cards() -> Vec<PublishedCard> {
    let row = |instance_type, baseline_gbps, peak_gbps, network_performance, ena_queues| {
        PublishedCard {
            instance_type,
            baseline_gbps,
            peak_gbps,
            network_performance,
            ena_queues,
        }
    };
    vec![
        // The fifth generation Intel compute optimized ladder: four sizes that sustain the
        // family's own baseline and burst to the 10 Gigabit ceiling, two sizes carved out of
        // one half of a host that share a dedicated 12, and three sizes that carry the whole
        // host's 25 — a ladder no share of the host reproduces, since the narrowest size
        // sustains more than its vCPUs' share and the `18xlarge` the whole figure.
        row("c5.large", 0.75, 10.0, "Up to 10 Gigabit", 8),
        row("c5.xlarge", 1.25, 10.0, "Up to 10 Gigabit", 8),
        row("c5.2xlarge", 2.5, 10.0, "Up to 10 Gigabit", 8),
        row("c5.4xlarge", 5.0, 10.0, "Up to 10 Gigabit", 8),
        row("c5.9xlarge", 12.0, 12.0, "12 Gigabit", 8),
        row("c5.12xlarge", 12.0, 12.0, "12 Gigabit", 8),
        row("c5.18xlarge", 25.0, 25.0, "25 Gigabit", 8),
        row("c5.24xlarge", 25.0, 25.0, "25 Gigabit", 8),
        row("c5.metal", 25.0, 25.0, "25 Gigabit", 8),
        // The first generation AMD EPYC memory optimized ladder, on a 20 Gigabit host: the
        // same baseline ladder at the narrow end, its OWN figure on the `8xlarge` (which
        // still bursts above it), and dedicated figures of 10, 12 and 20 above that.
        row("r5a.large", 0.75, 10.0, "Up to 10 Gigabit", 8),
        row("r5a.xlarge", 1.25, 10.0, "Up to 10 Gigabit", 8),
        row("r5a.2xlarge", 2.5, 10.0, "Up to 10 Gigabit", 8),
        row("r5a.4xlarge", 5.0, 10.0, "Up to 10 Gigabit", 8),
        row("r5a.8xlarge", 7.5, 10.0, "Up to 10 Gigabit", 8),
        row("r5a.12xlarge", 10.0, 10.0, "10 Gigabit", 8),
        row("r5a.16xlarge", 12.0, 12.0, "12 Gigabit", 8),
        row("r5a.24xlarge", 20.0, 20.0, "20 Gigabit", 8),
        // The general purpose ladder of the same Intel host, with and without local devices:
        // one family's narrow sizes are given FEWER queues than the eight its adapter
        // carries, which is why the queue count is a published figure rather than a constant.
        row("m5.large", 0.75, 10.0, "Up to 10 Gigabit", 2),
        row("m5.xlarge", 1.25, 10.0, "Up to 10 Gigabit", 4),
        row("m5.8xlarge", 10.0, 10.0, "10 Gigabit", 8),
        row("m5.24xlarge", 25.0, 25.0, "25 Gigabit", 8),
        row("m5d.large", 0.75, 10.0, "Up to 10 Gigabit", 8),
        row("m5d.12xlarge", 12.0, 12.0, "12 Gigabit", 8),
        row("m5d.metal", 25.0, 25.0, "25 Gigabit", 8),
        // The network optimized family of that generation, on a 100 Gigabit host: baselines
        // that are not whole Gigabit at all, a burst ceiling well above 10, and a queue count
        // that GROWS with the size up to the adapter's thirty-two.
        row("m5n.large", 2.1, 25.0, "Up to 25 Gigabit", 8),
        row("m5n.2xlarge", 8.125, 25.0, "Up to 25 Gigabit", 16),
        row("m5n.4xlarge", 16.25, 25.0, "Up to 25 Gigabit", 32),
        row("m5n.12xlarge", 50.0, 50.0, "50 Gigabit", 32),
    ]
}

/// The one card `instance_type` reports, as (baseline Gbps, peak Gbps, text, ENA queues).
async fn reported_card(
    router: &MultiServiceRouter,
    instance_type: &str,
) -> (f64, f64, String, Option<i64>) {
    let case = instance_type;
    let body = described_instance_type(case, router, REGION, instance_type).await;
    let number = |member: &str| -> f64 {
        let path = ["networkInfo", "networkCards", "item", member];
        let reported = reported_instance_type_member(case, &body, &path).unwrap_or_else(|| {
            panic!("case {case:?}: the one network card reports {member}, got {body}")
        });
        reported
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: {member} is a number, got {reported:?}"))
    };
    let path = ["networkInfo", "networkCards", "item", "networkPerformance"];
    let performance = reported_instance_type_member(case, &body, &path).unwrap_or_else(|| {
        panic!("case {case:?}: the one network card reports its networkPerformance, got {body}")
    });
    let path = [
        "networkInfo",
        "networkCards",
        "item",
        "defaultEnaQueueCountPerInterface",
    ];
    let queues = reported_instance_type_member(case, &body, &path).map(|queues| {
        queues
            .parse::<i64>()
            .unwrap_or_else(|_| panic!("case {case:?}: a queue count, got {queues:?}"))
    });
    (
        number("baselineBandwidthInGbps"),
        number("peakBandwidthInGbps"),
        performance,
        queues,
    )
}

/// Rule: each of the four card members is the figure published for that instance type.
#[tokio::test]
async fn network_card_rows_are_published_per_instance_type() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in published_cards() {
        let case = row.instance_type;
        let (baseline, peak, performance, queues) = reported_card(&router, case).await;
        assert!(
            (baseline - row.baseline_gbps).abs() < BANDWIDTH_TOLERANCE,
            "case {case:?}: expected baselineBandwidthInGbps {}, got {baseline}",
            row.baseline_gbps
        );
        assert!(
            (peak - row.peak_gbps).abs() < BANDWIDTH_TOLERANCE,
            "case {case:?}: expected peakBandwidthInGbps {}, got {peak}",
            row.peak_gbps
        );
        assert_eq!(
            performance, row.network_performance,
            "case {case:?}: expected the card's networkPerformance {:?}, got {performance:?}",
            row.network_performance
        );
        assert_eq!(
            queues,
            Some(row.ena_queues),
            "case {case:?}: expected defaultEnaQueueCountPerInterface {:?}, got {queues:?}",
            row.ena_queues
        );
    }
}

/// Rule, the invariants every row must hold: a card's text is the text OF its own two figures,
/// the type reports the same performance its one card does, and a size that bursts reports a
/// peak above its baseline while a size with a dedicated figure reports the two equal.
#[tokio::test]
async fn a_card_cannot_disagree_with_its_own_figures() {
    let (router, _engine) = super::ec2_query::emulator();

    for row in published_cards() {
        let case = row.instance_type;
        let (baseline, peak, performance, _) = reported_card(&router, case).await;
        let bursts = performance.starts_with("Up to ");
        assert_eq!(
            bursts,
            peak - baseline > BANDWIDTH_TOLERANCE,
            "case {case:?}: {performance:?} must say \"Up to\" exactly when the peak {peak} is \
             above the baseline {baseline}"
        );
        let figure = if bursts { peak } else { baseline };
        let written = performance
            .trim_start_matches("Up to ")
            .trim_end_matches(" Gigabit");
        let written: f64 = written
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: a Gigabit figure, got {performance:?}"));
        assert!(
            (written - figure).abs() < 0.01,
            "case {case:?}: {performance:?} must write the figure {figure}, got {written}"
        );

        let body = described_instance_type(case, &router, REGION, case).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "networkPerformance");
        assert_eq!(
            reported, performance,
            "case {case:?}: the type and its one card report one networkPerformance, got \
             {reported:?} beside {performance:?}"
        );
    }
}

/// Rule, the sharp edge: the published card is NOT a function of the vCPUs the name gives.
/// Three pairs of families meet at one size and disagree — on the figures, on whether the size
/// bursts at all, and on the queues one interface is given.
#[tokio::test]
async fn two_families_at_one_size_publish_different_cards() {
    let (router, _engine) = super::ec2_query::emulator();

    for (left, right) in [
        // 96 vCPUs each: the whole of a 25 Gigabit host beside the whole of a 20 Gigabit one.
        ("c5.24xlarge", "r5a.24xlarge"),
        // 32 vCPUs each: one sustains a dedicated 10, the other 7.5 and bursts above it.
        ("m5.8xlarge", "r5a.8xlarge"),
        // 2 vCPUs each: one sustains 0.75 of a 25 Gigabit host with two queues per interface,
        // the other 2.1 of a 100 Gigabit one with eight.
        ("m5.large", "m5n.large"),
    ] {
        let case = format!("{left} beside {right}");
        let (left_baseline, left_peak, left_text, left_queues) = reported_card(&router, left).await;
        let (right_baseline, right_peak, right_text, right_queues) =
            reported_card(&router, right).await;
        let left_vcpus = described_vcpus(&router, left).await;
        let right_vcpus = described_vcpus(&router, right).await;
        assert_eq!(
            left_vcpus, right_vcpus,
            "case {case:?}: both rows must be the same size, got {left_vcpus} and {right_vcpus} \
             vCPUs"
        );
        assert_ne!(
            (
                format!("{left_baseline}"),
                format!("{left_peak}"),
                left_text.clone(),
                left_queues
            ),
            (
                format!("{right_baseline}"),
                format!("{right_peak}"),
                right_text.clone(),
                right_queues
            ),
            "case {case:?}: two families of one size publish different cards"
        );
    }
}

/// The vCPUs `instance_type` reports, which is what makes two rows the same SIZE.
async fn described_vcpus(router: &MultiServiceRouter, instance_type: &str) -> i64 {
    let case = instance_type;
    let body = described_instance_type(case, router, REGION, instance_type).await;
    let vcpu_info = required(case, &body, "vCpuInfo");
    let reported = required(case, &vcpu_info, "defaultVCpus");
    reported
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: a vCPU count, got {reported:?}"))
}
