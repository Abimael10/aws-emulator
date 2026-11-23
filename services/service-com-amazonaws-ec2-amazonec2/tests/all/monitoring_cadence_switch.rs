// SPDX-License-Identifier: CC0-1.0

//! The cadence SWITCH: what `MonitorInstances` and `UnmonitorInstances` do to the periods an
//! instance's `AWS/EC2` series are published into, from the moment the move they start settles.
//!
//! THE RULE: a running instance publishes each of its guest measurements into `AWS/EC2`,
//! dimensioned on exactly `{InstanceId}` and in that metric's own unit, at the period its
//! CURRENT monitoring state selects — one minute while detailed monitoring is in force
//! ("Detailed monitoring | You can get metrics in 1-minute periods") and five minutes while it
//! is off ("All other metrics are available in 5-minute periods"), with the status-check metrics
//! on one minute in either state ("Basic monitoring | Status check metrics are available in
//! 1-minute periods"; Amazon EC2 User Guide, instance monitoring). The cadence is therefore
//! decided AT INGEST and changes when the monitoring move settles, for the instances the call
//! NAMED and no other: each period is a wall-clock window anchored at a multiple of its own
//! length, stamped with that window's start, so one identical 300-second window holds one
//! datapoint of a 5-minute series and one per minute of a 1-minute one — and a stamp off the
//! 5-minute grid can only have come from a 1-minute period. A later `Period=60` read cannot
//! invent a bucket that was never stored, which is what makes the two cadences tell a detailed
//! instance from a basic twin.
//!
//! This is the SWITCH itself, which the sibling suites do not cover: `instance_metric_publication`
//! settles each monitoring state before it drives a span and reads one instance per span, and
//! `instance_cpu_metric_read_periods` reads two instances that were already in their final states.
//! Here two twins launched by ONE `RunInstances` with `Monitoring` omitted are driven through a
//! single span of periods with the switch in the MIDDLE of it, and the two columns are then read
//! over one identical 300-second window that lies wholly after the switch — the comparison a
//! client makes to tell detailed monitoring from basic.
//!
//! The rows span what the rule generalises over: the cadence class (guest measurements, whose
//! period the monitoring state selects, and a status check, whose period it does not), the metric
//! and the unit each is measured in. The cases span the direction of the move (`MonitorInstances`
//! shortening the period and `UnmonitorInstances` restoring it) and how many of a set the call
//! names (one of two, and two of three). Every expected stamp, unit and period is spelled out
//! here rather than imported, so a wrong constant in the implementation cannot make a case pass.
//!
//! The harness is the sibling suite's, helpers and all: Amazon CloudWatch mounted BESIDE Amazon
//! EC2 on one engine, as the assembled router mounts them, with the span driven by advancing the
//! sampler's own clock rather than by waiting for those periods to pass, and every datapoint read
//! back through CloudWatch's own `GetMetricStatistics`.

use smithy_server_runtime::MultiServiceRouter;

use super::instance_metric_publication::{
    datapoints, ec2_ok, emulator, launch, p, poll_until_reported, sample_minutes, span_base,
    Datapoint, FIVE_MINUTES, MINUTE,
};

/// Which minute of the span the monitoring call is made at: a multiple of five, so the window
/// before it and the window after it are both whole periods of both cadences.
const SWITCH_MINUTE: i64 = 10;

/// Which of the sibling suite's phases this suite lays its span over: one far enough ahead of
/// the wall clock that the service's own sampler, which publishes the periods the WALL clock has
/// closed, cannot reach the span while a case is running — so the only readings inside it are the
/// ones this suite's own sampling took.
const SPAN_PHASE: i64 = 1;

/// How many minutes of the span are sampled in all — wide enough that the 300-second window
/// after the switch is closed and so has every reading it owes.
const SPAN_MINUTES: i64 = 20;

/// The monitoring states `MonitorInstances` may report for an instance it just named: the
/// transient one it enters at once, or the settled one if the move has already come due.
const ENABLING_STATES: [&str; 2] = ["pending", "enabled"];

/// The monitoring states `UnmonitorInstances` may report for an instance it just named.
const DISABLING_STATES: [&str; 2] = ["disabling", "disabled"];

/// The fewest datapoints a 1-minute series must yield over one 300-second window for a client
/// to call it detailed monitoring, and the most a 5-minute series may yield over the same
/// window: the discriminator the cadence comparison is made on.
const DETAILED_MINIMUM: usize = 3;

/// The most datapoints the 5-minute column may yield over that same window.
const BASIC_MAXIMUM: usize = 1;

/// One row of the table: a metric a running instance publishes, the unit it is measured in and
/// which minutes of one 300-second window its series carries a datapoint in under each
/// monitoring state, as offsets in whole minutes from the start of that window.
///
/// The catalog, the units and the two cadences come from the metrics Amazon EC2 documents for
/// one INSTANCE (Amazon EC2 User Guide, "Instance metrics" and "Status check metrics") and from
/// the monitoring sentences the module docs quote; they are documented metrics rather than
/// Smithy shapes, so there is no model member to read them from.
struct CadenceRow {
    /// The `MetricName` the series is published under.
    metric: &'static str,
    /// The `StandardUnit` its datapoints must carry.
    unit: &'static str,
    /// Whether the monitoring state is what selects this metric's period at all.
    switches_with_monitoring: bool,
    /// The minutes of the window its series carries a datapoint in while detailed monitoring is
    /// in force.
    detailed_minutes: &'static [i64],
    /// The minutes it carries a datapoint in while detailed monitoring is off.
    basic_minutes: &'static [i64],
}

/// Every minute of a 300-second window: what a 1-minute series carries a datapoint in.
const EVERY_MINUTE: &[i64] = &[0, 1, 2, 3, 4];

/// The first minute of it alone: the one datapoint a 5-minute series carries, stamped with the
/// window's own start.
const THE_FIRST_MINUTE_ALONE: &[i64] = &[0];

/// The rows this suite drives: the CPU series the cadence comparison is made on, two more guest
/// measurements in the other units of the catalog, and one status-check metric, whose period the
/// monitoring state does not select.
const CADENCE_ROWS: &[CadenceRow] = &[
    CadenceRow {
        metric: "CPUUtilization",
        unit: "Percent",
        switches_with_monitoring: true,
        detailed_minutes: EVERY_MINUTE,
        basic_minutes: THE_FIRST_MINUTE_ALONE,
    },
    CadenceRow {
        metric: "NetworkIn",
        unit: "Bytes",
        switches_with_monitoring: true,
        detailed_minutes: EVERY_MINUTE,
        basic_minutes: THE_FIRST_MINUTE_ALONE,
    },
    CadenceRow {
        metric: "DiskReadOps",
        unit: "Count",
        switches_with_monitoring: true,
        detailed_minutes: EVERY_MINUTE,
        basic_minutes: THE_FIRST_MINUTE_ALONE,
    },
    // "Status check metrics are available in 1-minute periods" under basic monitoring too, so
    // this row's window holds a datapoint per minute whichever state the instance is in — the
    // monitoring move moves nothing about it.
    CadenceRow {
        metric: "StatusCheckFailed_System",
        unit: "Count",
        switches_with_monitoring: false,
        detailed_minutes: EVERY_MINUTE,
        basic_minutes: EVERY_MINUTE,
    },
];

/// Which cadence an instance is being read under, which is what picks a row's column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cadence {
    /// Detailed monitoring in force.
    Detailed,
    /// Detailed monitoring off.
    Basic,
}

impl CadenceRow {
    /// The minutes of one 300-second window this row's series carries a datapoint in, under
    /// `cadence`.
    fn minutes(&self, cadence: Cadence) -> &'static [i64] {
        match cadence {
            Cadence::Detailed => self.detailed_minutes,
            Cadence::Basic => self.basic_minutes,
        }
    }
}

/// The stamps of `readings` as offsets in whole minutes from `from`.
fn minute_offsets(readings: &[Datapoint], from: i64) -> Vec<i64> {
    readings
        .iter()
        .map(|reading| (reading.at - from) / MINUTE)
        .collect()
}

/// Drive one monitoring call over the instances `instance_ids` names and answer the
/// `(instanceId, monitoring state)` pairs it reported, sorted.
async fn monitoring_call(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    instance_ids: &[String],
) -> Vec<(String, String)> {
    let params: Vec<(String, String)> = instance_ids
        .iter()
        .enumerate()
        .map(|(index, instance_id)| p(&format!("InstanceId.{}", index + 1), instance_id.clone()))
        .collect();
    let body = ec2_ok(router, case, action, &params).await;
    let parsed = super::wire::document(case, &body);
    let mut reported: Vec<(String, String)> =
        super::wire::set_items(parsed.root_element(), "instancesSet")
            .into_iter()
            .map(|item| {
                (
                    super::wire::child_text(item, "instanceId").unwrap_or_default(),
                    super::wire::child(item, "monitoring")
                        .and_then(|monitoring| super::wire::child_text(monitoring, "state"))
                        .unwrap_or_default(),
                )
            })
            .collect();
    reported.sort();
    reported
}

/// Assert that `instance_id` carries exactly the datapoints every row of the table owes under
/// `cadence` inside the 300-second window beginning at `window_start`.
async fn assert_window(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    cadence: Cadence,
    window_start: i64,
) {
    for row in CADENCE_ROWS {
        let case = format!("{case}: {} of {instance_id} under {cadence:?}", row.metric);
        let readings = datapoints(
            router,
            &case,
            instance_id,
            row.metric,
            window_start,
            window_start + FIVE_MINUTES,
        )
        .await;
        let expected = row.minutes(cadence);
        assert_eq!(
            minute_offsets(&readings, window_start).as_slice(),
            expected,
            "case {case:?}: expected a datapoint in the minutes {expected:?} of the \
             {FIVE_MINUTES}-second window at {window_start}, got {readings:?}"
        );
        for reading in &readings {
            assert_eq!(
                reading.unit, row.unit,
                "case {case:?}: expected the unit {:?}, got {reading:?}",
                row.unit
            );
            assert_eq!(
                reading.at.rem_euclid(MINUTE),
                0,
                "case {case:?}: a period of either cadence begins on a whole minute, got \
                 {reading:?}"
            );
        }
        // The discriminator itself, asserted of the metrics whose period the monitoring state
        // selects: a 1-minute series yields several datapoints over the window and some of them
        // off the 5-minute grid, a 5-minute series at most the one on that grid.
        if !row.switches_with_monitoring {
            continue;
        }
        let off_grid: Vec<&Datapoint> = readings
            .iter()
            .filter(|reading| reading.at.rem_euclid(FIVE_MINUTES) != 0)
            .collect();
        match cadence {
            Cadence::Detailed => {
                assert!(
                    readings.len() >= DETAILED_MINIMUM,
                    "case {case:?}: detailed monitoring must yield at least {DETAILED_MINIMUM} \
                     datapoints over one {FIVE_MINUTES}-second window, got {readings:?}"
                );
                assert!(
                    !off_grid.is_empty(),
                    "case {case:?}: a 1-minute period begins off the {FIVE_MINUTES}-second \
                     grid, so some stamp must be off it, got {readings:?}"
                );
            }
            Cadence::Basic => {
                assert!(
                    readings.len() <= BASIC_MAXIMUM,
                    "case {case:?}: basic monitoring must yield at most {BASIC_MAXIMUM} \
                     datapoint over one {FIVE_MINUTES}-second window, got {readings:?}"
                );
                assert!(
                    off_grid.is_empty(),
                    "case {case:?}: a 5-minute period begins on a {FIVE_MINUTES}-second \
                     boundary, so no stamp may be off that grid, got {readings:?}"
                );
            }
        }
    }
}

/// Rule: `MonitorInstances` shortens the publication period of the guest measurements of the
/// instances it NAMES to one minute once the move it starts settles, and of no other instance —
/// so over one identical 300-second window after the switch the named instance yields a datapoint
/// per minute, with stamps off the 5-minute grid, while a twin launched beside it by the same
/// `RunInstances` call still yields the single datapoint of its 5-minute period.
#[tokio::test]
async fn monitor_instances_shortens_the_period_of_the_instance_it_names_only() {
    let case = "twins from one launch, one of them monitored mid-span";
    let (router, engine) = emulator();
    let ids = launch(&router, case, 2).await;
    let (twin, named) = (ids[0].clone(), ids[1].clone());
    // `Monitoring` was omitted from the launch, so both twins are running under basic
    // monitoring before anything switches.
    poll_until_reported(&router, case, &twin, "running", "disabled").await;
    poll_until_reported(&router, case, &named, "running", "disabled").await;

    let base = span_base(SPAN_PHASE);
    let switch_at = base + SWITCH_MINUTE * MINUTE;
    // The span up to the switch: both twins are under basic monitoring for every period of it.
    sample_minutes(&engine, base, 1..SWITCH_MINUTE + 1);
    for instance_id in [&twin, &named] {
        assert_window(
            &router,
            &format!("{case}, before the switch"),
            instance_id,
            Cadence::Basic,
            switch_at - FIVE_MINUTES,
        )
        .await;
    }

    let reported =
        monitoring_call(&router, case, "MonitorInstances", std::slice::from_ref(&named)).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: MonitorInstances reports the instances it acted on, which is the one it \
         named, got {reported:?}"
    );
    assert_eq!(
        reported[0].0, named,
        "case {case:?}: the reported instance is the one named, got {reported:?}"
    );
    assert!(
        ENABLING_STATES.contains(&reported[0].1.as_str()),
        "case {case:?}: expected a monitoring state in {ENABLING_STATES:?}, got {reported:?}"
    );
    // The twin was not named, so nothing of it moved.
    poll_until_reported(&router, case, &twin, "running", "disabled").await;
    poll_until_reported(&router, case, &named, "running", "enabled").await;

    // The rest of the span, with the switch settled: the 300-second window after it is where the
    // two cadences are compared.
    sample_minutes(&engine, base, SWITCH_MINUTE + 1..SPAN_MINUTES + 1);
    assert_window(
        &router,
        &format!("{case}, after the switch"),
        &named,
        Cadence::Detailed,
        switch_at,
    )
    .await;
    assert_window(
        &router,
        &format!("{case}, after the switch"),
        &twin,
        Cadence::Basic,
        switch_at,
    )
    .await;
}

/// Rule: the same move the other way — `UnmonitorInstances` restores the 5-minute period of the
/// guest measurements of the instances it NAMES, and of no other, so the instance it names falls
/// back to one datapoint per 300-second window while a twin left under detailed monitoring keeps
/// publishing every minute.
#[tokio::test]
async fn unmonitor_instances_restores_the_period_of_the_instance_it_names_only() {
    let case = "two monitored instances, one of them unmonitored mid-span";
    let (router, engine) = emulator();
    let ids = launch(&router, case, 2).await;
    let (twin, named) = (ids[0].clone(), ids[1].clone());
    monitoring_call(
        &router,
        case,
        "MonitorInstances",
        &[twin.clone(), named.clone()],
    )
    .await;
    poll_until_reported(&router, case, &twin, "running", "enabled").await;
    poll_until_reported(&router, case, &named, "running", "enabled").await;

    let base = span_base(SPAN_PHASE);
    let switch_at = base + SWITCH_MINUTE * MINUTE;
    // The span up to the switch: both instances are under detailed monitoring for it.
    sample_minutes(&engine, base, 1..SWITCH_MINUTE + 1);
    for instance_id in [&twin, &named] {
        assert_window(
            &router,
            &format!("{case}, before the switch"),
            instance_id,
            Cadence::Detailed,
            switch_at - FIVE_MINUTES,
        )
        .await;
    }

    let reported =
        monitoring_call(&router, case, "UnmonitorInstances", std::slice::from_ref(&named)).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: UnmonitorInstances reports the instances it acted on, which is the one \
         it named, got {reported:?}"
    );
    assert_eq!(
        reported[0].0, named,
        "case {case:?}: the reported instance is the one named, got {reported:?}"
    );
    assert!(
        DISABLING_STATES.contains(&reported[0].1.as_str()),
        "case {case:?}: expected a monitoring state in {DISABLING_STATES:?}, got {reported:?}"
    );
    poll_until_reported(&router, case, &twin, "running", "enabled").await;
    poll_until_reported(&router, case, &named, "running", "disabled").await;

    sample_minutes(&engine, base, SWITCH_MINUTE + 1..SPAN_MINUTES + 1);
    assert_window(
        &router,
        &format!("{case}, after the switch"),
        &named,
        Cadence::Basic,
        switch_at,
    )
    .await;
    assert_window(
        &router,
        &format!("{case}, after the switch"),
        &twin,
        Cadence::Detailed,
        switch_at,
    )
    .await;
}

/// Rule: the switch is per instance NAMED, however many of a set the call names — two of three
/// instances launched together publish every minute while the third, which the call did not
/// name, keeps its 5-minute period.
#[tokio::test]
async fn the_switch_reaches_exactly_the_named_instances_of_a_larger_set() {
    let case = "three instances from one launch, two of them monitored";
    let (router, engine) = emulator();
    let ids = launch(&router, case, 3).await;
    let (first, untouched, third) = (ids[0].clone(), ids[1].clone(), ids[2].clone());
    let reported = monitoring_call(
        &router,
        case,
        "MonitorInstances",
        &[first.clone(), third.clone()],
    )
    .await;
    let named_ids: Vec<String> = reported
        .iter()
        .map(|(instance_id, _)| instance_id.clone())
        .collect();
    let mut expected_ids = vec![first.clone(), third.clone()];
    expected_ids.sort();
    assert_eq!(
        named_ids, expected_ids,
        "case {case:?}: MonitorInstances reports exactly the instances it named, got {reported:?}"
    );
    for (instance_id, state) in &reported {
        assert!(
            ENABLING_STATES.contains(&state.as_str()),
            "case {case:?}: expected {instance_id} in a state of {ENABLING_STATES:?}, got \
             {reported:?}"
        );
    }
    poll_until_reported(&router, case, &first, "running", "enabled").await;
    poll_until_reported(&router, case, &third, "running", "enabled").await;
    poll_until_reported(&router, case, &untouched, "running", "disabled").await;

    let base = span_base(SPAN_PHASE);
    sample_minutes(&engine, base, 1..SWITCH_MINUTE + 1);
    // The last whole 300-second window of the sampled span, read for all three.
    let window_start = base + SWITCH_MINUTE * MINUTE - FIVE_MINUTES;
    for (instance_id, cadence) in [
        (&first, Cadence::Detailed),
        (&third, Cadence::Detailed),
        (&untouched, Cadence::Basic),
    ] {
        assert_window(&router, case, instance_id, cadence, window_start).await;
    }
}
