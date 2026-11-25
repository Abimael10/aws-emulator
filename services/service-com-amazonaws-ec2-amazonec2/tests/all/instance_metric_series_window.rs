// SPDX-License-Identifier: CC0-1.0

//! WHEN an instance's `AWS/EC2` series begins and ends, as a client reading that series from
//! the instance's own launch sees it.
//!
//! THE RULE: a reading of an instance's guest is TAKEN at a boundary of the publication period
//! its monitoring state selects — one minute while detailed monitoring is in force ("Detailed
//! monitoring | You can get metrics in 1-minute periods"), five minutes while it is off ("All
//! other metrics are available in 5-minute periods") and one minute for the status-check metrics
//! in either state ("Basic monitoring | Status check metrics are available in 1-minute periods";
//! Amazon EC2 User Guide, instance monitoring) — and the datapoint it produces carries THE INSTANT
//! IT WAS TAKEN AT. So every datapoint of an instance's series is stamped with an instant at which
//! that instance's guest was up: the series carries nothing stamped before the instance came up
//! and nothing stamped after it went away, and the first reading of a freshly launched instance is
//! therefore taken at the FIRST boundary of its period after it came up — one period after the
//! launch, not two.
//!
//! That is what a client waiting for an instance's first datapoint observes, because it reads the
//! series from the instance's own launch: a reading stamped before the launch is not in the window
//! it asked for at all, so a series whose readings are labelled one period early is empty to such
//! a reader until a whole further period has passed.
//!
//! The rows span what the rule generalises over: the monitoring state and so the period (five
//! minutes under basic monitoring, one minute under detailed), how detailed monitoring was reached
//! (asked for at the launch, or turned on afterwards by `MonitorInstances`), the cadence class (a
//! guest measurement, whose period the monitoring state selects, and a status check, whose period
//! it does not) and the metric and unit each is measured in. The catalog, the units and the two
//! cadences come from the metrics Amazon EC2 documents for one INSTANCE (Amazon EC2 User Guide,
//! "Instance metrics" and "Status check metrics") and from the monitoring sentences quoted above;
//! they are documented metrics rather than Smithy shapes, so there is no model member to read them
//! from. Every expected period and unit is spelled out here rather than imported.
//!
//! The harness is the sibling suite's ([`super::instance_metric_publication`]): Amazon CloudWatch
//! mounted BESIDE Amazon EC2 on one engine, as the assembled router mounts them, every datapoint
//! read back through CloudWatch's own `GetMetricStatistics`, and the sampler's own clock advanced
//! tick by tick — the ticks the service's clock takes on the wall clock — rather than waited for,
//! so a 5-minute period is covered without spending five minutes on it. Each read opens WELL
//! BEFORE the launch, so a datapoint stamped before the instance existed would be seen rather than
//! hidden by the window.

use smithy_server_runtime::MultiServiceRouter;

use super::instance_metric_publication::{
    datapoints, ec2_ok, emulator, launch, now_secs, p, poll_until_reported, sample_at, Datapoint,
    FIVE_MINUTES, MINUTE,
};

/// The image the launch that asks for detailed monitoring names, as the sibling suite's launches
/// name it.
const AMI: &str = "ami-0123456789abcdef0";

/// How often the service's own sampler looks for a reading that has come due, in seconds: the
/// step this suite advances that sampler's clock by, so what it drives is the sequence of looks a
/// running emulator takes rather than one look per period.
const SAMPLE_TICK: i64 = 5;

/// How much slack a row allows on top of its own period before it calls the first reading late,
/// in seconds: a few ticks, so the look that finds a boundary is never what decides a case.
const TICK_SLACK: i64 = 4 * SAMPLE_TICK;

/// How far back of the launch every read opens, in seconds.
///
/// Many periods of the longest cadence, so a datapoint stamped before the instance came up is
/// INSIDE the window a case reads and is caught by the assertions rather than cut off by the
/// window's own start.
const READ_FROM_BEFORE_LAUNCH: i64 = 6 * FIVE_MINUTES;

/// How far past the launch every read closes, in seconds: past the whole span any row drives.
const READ_UNTIL_AFTER_LAUNCH: i64 = 12 * FIVE_MINUTES;

/// How the instance of a row reached the monitoring state it publishes under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Monitoring {
    /// Detailed monitoring off, which is what a launch that says nothing about it leaves.
    Basic,
    /// Detailed monitoring asked for at the launch (`Monitoring.Enabled=true`).
    DetailedAtLaunch,
    /// Detailed monitoring turned on afterwards, by `MonitorInstances`.
    DetailedByCall,
}

impl Monitoring {
    /// The `Monitoring.State` `DescribeInstances` reports once the move this case makes has
    /// settled.
    fn settled_state(self) -> &'static str {
        match self {
            Self::Basic => "disabled",
            Self::DetailedAtLaunch | Self::DetailedByCall => "enabled",
        }
    }
}

/// One row of the table: a metric, the unit it is measured in, the monitoring state the instance
/// publishes it under and the period its readings are taken at under that state.
struct Row {
    /// What the row is called in a failure.
    case: &'static str,
    /// How the instance reached its monitoring state.
    monitoring: Monitoring,
    /// The `MetricName` the series is published under.
    metric: &'static str,
    /// The `StandardUnit` its datapoints must carry.
    unit: &'static str,
    /// The period its readings are taken at, in seconds.
    period: i64,
}

/// The rows this suite drives: both cadences of a guest measurement, a second guest measurement
/// in another unit, both ways into detailed monitoring, and a status check, whose period the
/// monitoring state does not select.
const ROWS: &[Row] = &[
    Row {
        case: "the CPU series of an instance under basic monitoring",
        monitoring: Monitoring::Basic,
        metric: "CPUUtilization",
        unit: "Percent",
        period: FIVE_MINUTES,
    },
    Row {
        case: "the network series of an instance under basic monitoring",
        monitoring: Monitoring::Basic,
        metric: "NetworkIn",
        unit: "Bytes",
        period: FIVE_MINUTES,
    },
    Row {
        case: "the CPU series of an instance launched with detailed monitoring",
        monitoring: Monitoring::DetailedAtLaunch,
        metric: "CPUUtilization",
        unit: "Percent",
        period: MINUTE,
    },
    Row {
        case: "the CPU series of an instance moved to detailed monitoring",
        monitoring: Monitoring::DetailedByCall,
        metric: "CPUUtilization",
        unit: "Percent",
        period: MINUTE,
    },
    // "Status check metrics are available in 1-minute periods" under basic monitoring too, so
    // this row's period is a minute whichever state the instance is in.
    Row {
        case: "a status check of an instance under basic monitoring",
        monitoring: Monitoring::Basic,
        metric: "StatusCheckFailed_System",
        unit: "Count",
        period: MINUTE,
    },
    Row {
        case: "a status check of an instance under detailed monitoring",
        monitoring: Monitoring::DetailedByCall,
        metric: "StatusCheckFailed_System",
        unit: "Count",
        period: MINUTE,
    },
];

/// An instance running under the monitoring state `monitoring` names, and the instant its launch
/// was asked for.
///
/// The instant is taken BEFORE the launch call, so it is at or before the launch the service
/// recorded — which is what makes "nothing stamped before the launch" an assertion that cannot
/// pass by a rounding.
async fn running_instance(
    router: &MultiServiceRouter,
    case: &str,
    monitoring: Monitoring,
) -> (String, i64) {
    let launched_at = now_secs();
    let instance_id = if monitoring == Monitoring::DetailedAtLaunch {
        let body = ec2_ok(
            router,
            case,
            "RunInstances",
            &[
                p("ImageId", AMI),
                p("MinCount", "1"),
                p("MaxCount", "1"),
                p("Monitoring.Enabled", "true"),
            ],
        )
        .await;
        super::ec2_query::element(&body, "instanceId").unwrap_or_else(|| {
            panic!("case {case:?}: RunInstances must report the instance it minted: {body}")
        })
    } else {
        launch(router, case, 1).await.remove(0)
    };
    if monitoring == Monitoring::DetailedByCall {
        ec2_ok(
            router,
            case,
            "MonitorInstances",
            &[p("InstanceId.1", instance_id.clone())],
        )
        .await;
    }
    poll_until_reported(
        router,
        case,
        &instance_id,
        "running",
        monitoring.settled_state(),
    )
    .await;
    (instance_id, launched_at)
}

/// One instance's series under this suite's own sampler: what a search for a reading needs to
/// know, so one search is a method rather than a function of eight parameters.
struct Series<'a> {
    /// The assembled pair of services the reads go through.
    router: &'a MultiServiceRouter,
    /// The engine whose sampler this suite advances.
    engine: &'a engine::InMemoryEngine,
    /// What the search is called in a failure.
    case: &'a str,
    /// The instance the series is dimensioned on.
    instance_id: &'a str,
    /// The metric the series is of.
    metric: &'a str,
    /// The instant the instance's launch was asked for, which every read opens well before.
    launched_at: i64,
}

impl Series<'_> {
    /// The datapoints of this series, read at one-minute granularity over a window that OPENS
    /// WELL BEFORE the launch — so a reading stamped before the instance came up is reported
    /// rather than hidden.
    async fn read(&self) -> Vec<Datapoint> {
        datapoints(
            self.router,
            self.case,
            self.instance_id,
            self.metric,
            self.launched_at - READ_FROM_BEFORE_LAUNCH,
            self.launched_at + READ_UNTIL_AFTER_LAUNCH,
        )
        .await
    }

    /// Advance the sampler tick by tick from `from` and answer the offset from `from` at which
    /// this series first carried a datapoint, with the series as it then stood.
    ///
    /// `None` is "no datapoint inside `bound` seconds of `from`", which is how a row reports a
    /// series that begins later than the rule allows.
    async fn first_reading(&self, from: i64, bound: i64) -> (Option<i64>, Vec<Datapoint>) {
        let mut offset = SAMPLE_TICK;
        while offset <= bound {
            sample_at(self.engine, from + offset);
            let points = self.read().await;
            if !points.is_empty() {
                return (Some(offset), points);
            }
            offset += SAMPLE_TICK;
        }
        (None, self.read().await)
    }
}

/// Rule: every datapoint of an instance's series is stamped with an instant at which that
/// instance was up, so the first reading of a freshly launched instance is taken at the first
/// boundary of its own period after the launch — within one period of it, and never before it.
#[tokio::test]
async fn the_series_of_a_launched_instance_begins_after_it_came_up() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = emulator();
        let (instance_id, launched_at) = running_instance(&router, case, row.monitoring).await;
        let running_at = now_secs();

        let series = Series {
            router: &router,
            engine: &engine,
            case,
            instance_id: &instance_id,
            metric: row.metric,
            launched_at,
        };
        let (found, points) = series
            .first_reading(running_at, row.period + TICK_SLACK)
            .await;
        let found = found.unwrap_or_else(|| {
            panic!(
                "case {case:?}: a reading is taken at every boundary of this {}-second period, so \
                 one is owed within {} seconds of the instance coming up; after {} seconds of the \
                 sampler's clock the series held {points:?}",
                row.period,
                row.period,
                row.period + TICK_SLACK
            )
        });
        assert!(
            found <= row.period + TICK_SLACK,
            "case {case:?}: expected the first reading within {} seconds of the instance coming \
             up, got it after {found}",
            row.period
        );
        let first = points
            .first()
            .expect("a non-empty series has a first datapoint");
        assert!(
            first.at > launched_at,
            "case {case:?}: a reading is stamped with the instant it was taken, and nothing was \
             measured before the launch at {launched_at}, got {first:?}"
        );
        assert_eq!(
            first.at.rem_euclid(row.period),
            0,
            "case {case:?}: a reading is taken at a boundary of its own {}-second period, got \
             {first:?}",
            row.period
        );
        assert!(
            first.at <= running_at + row.period,
            "case {case:?}: the first reading is taken at the FIRST boundary after the instance \
             came up at {running_at}, so it is stamped no later than one {}-second period past \
             it, got {first:?}",
            row.period
        );
        for point in &points {
            assert!(
                point.at > launched_at,
                "case {case:?}: no datapoint of this series may be stamped before the launch at \
                 {launched_at}, got {point:?} in {points:?}"
            );
            assert_eq!(
                point.unit, row.unit,
                "case {case:?}: expected the unit {:?}, got {point:?}",
                row.unit
            );
        }
    }
}

/// Rule, at the other end of the same series: an instance that is gone is being measured by
/// nothing, so no reading is ever stamped later than the last instant the sampler looked at while
/// it ran — and the readings taken while it ran are still there.
#[tokio::test]
async fn the_series_of_a_terminated_instance_gains_nothing_stamped_after_it_stopped() {
    let case = "an instance terminated after its first reading";
    let (router, engine) = emulator();
    let (instance_id, launched_at) =
        running_instance(&router, case, Monitoring::DetailedByCall).await;
    let running_at = now_secs();
    let series = Series {
        router: &router,
        engine: &engine,
        case,
        instance_id: &instance_id,
        metric: "CPUUtilization",
        launched_at,
    };
    let (found, while_running) = series.first_reading(running_at, MINUTE + TICK_SLACK).await;
    let found = found.unwrap_or_else(|| {
        panic!("case {case:?}: the instance must publish while it runs, got {while_running:?}")
    });
    // The last instant the sampler was asked about while the instance was still RUNNING: every
    // reading of it was taken at or before that instant, since a reading is taken at a boundary
    // the sampler's clock has passed.
    let last_look_while_running = running_at + found;

    ec2_ok(
        &router,
        case,
        "TerminateInstances",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    // Terminating an instance does not turn its detailed monitoring off — the move the clock
    // owes is the lifecycle one — so the settled monitoring state is still the one the call
    // above left.
    poll_until_reported(
        &router,
        case,
        &instance_id,
        "terminated",
        Monitoring::DetailedByCall.settled_state(),
    )
    .await;
    let terminated_at = now_secs();
    let kept = series.read().await;
    assert!(
        !kept.is_empty(),
        "case {case:?}: the readings taken while the instance ran are what the comparison below \
         is of, so there must be at least one"
    );

    // Several periods of the sampler's clock AFTER the instance is gone: nothing is measuring it,
    // so none of those boundaries carries a reading of it.
    for tick in 1..=(3 * MINUTE / SAMPLE_TICK) {
        sample_at(&engine, terminated_at + tick * SAMPLE_TICK);
    }
    let after = series.read().await;
    assert_eq!(
        after, kept,
        "case {case:?}: the series of a terminated instance keeps the readings taken while it \
         ran and gains no other, expected {kept:?}, got {after:?}"
    );
    for point in &after {
        assert!(
            point.at <= last_look_while_running,
            "case {case:?}: every reading was taken at a boundary the sampler had passed while \
             the instance ran, the last of which was {last_look_while_running}, got {point:?}"
        );
        assert!(
            point.at > launched_at,
            "case {case:?}: no reading may be stamped before the launch at {launched_at}, got \
             {point:?}"
        );
    }
}
