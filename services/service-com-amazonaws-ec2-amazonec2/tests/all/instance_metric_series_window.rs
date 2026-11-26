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
//! "After it came up" is after the series became MEASURABLE on that instance, which is what the
//! reading of a boundary needs to exist: a guest measurement is measurable as soon as the guest is
//! up, which the instance reporting `running` is, while a status check is measurable only once it
//! has reached a VERDICT — "it can take a few minutes for the instance to pass its status checks"
//! (Amazon EC2 User Guide, status checks), and a check still initializing has no verdict to publish
//! for the period it was looked at in. So each case waits for the CONDITION its own series needs,
//! asked of the service itself, and then asks for the first boundary past it; nothing here waits
//! for a span or asserts on how long anything took, so where an instance's boot happens to fall on
//! the period grid cannot decide a case.
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

use std::time::{SystemTime, UNIX_EPOCH};

use smithy_server_runtime::MultiServiceRouter;

use super::instance_metric_publication::{
    datapoints, ec2_ok, emulator, launch, now_secs, p, poll_until_reported, sample_at, Datapoint,
    FIVE_MINUTES, MINUTE,
};
use super::rbin_retention_rules::poll_until;

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

/// What a row's series needs of the instance before a reading of it can be TAKEN at a boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measurable {
    /// A quantity measured on the GUEST: it is measurable as soon as the guest is up, which is
    /// the instance reporting `running`.
    GuestUp,
    /// A status CHECK's verdict: a check that is still initializing has reached none, so the
    /// series begins at the first boundary after the check reports one.
    CheckVerdict,
}

/// The `SummaryStatus` values that ARE a verdict of a status check: a check reports `ok` for
/// the "0 (passed)" of "0 (passed) or 1 (failed)" and `impaired` for its "1 (failed)", while
/// `initializing` and `not-applicable` are no verdict at all (`DescribeInstanceStatus`'s
/// `SummaryStatus`, and Amazon EC2 User Guide, "Status check metrics").
const CHECK_VERDICTS: [&str; 2] = ["ok", "impaired"];

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
    /// What the instance owes before a reading of this series exists at all.
    measurable: Measurable,
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
        measurable: Measurable::GuestUp,
    },
    Row {
        case: "the network series of an instance under basic monitoring",
        monitoring: Monitoring::Basic,
        metric: "NetworkIn",
        unit: "Bytes",
        period: FIVE_MINUTES,
        measurable: Measurable::GuestUp,
    },
    Row {
        case: "the CPU series of an instance launched with detailed monitoring",
        monitoring: Monitoring::DetailedAtLaunch,
        metric: "CPUUtilization",
        unit: "Percent",
        period: MINUTE,
        measurable: Measurable::GuestUp,
    },
    Row {
        case: "the CPU series of an instance moved to detailed monitoring",
        monitoring: Monitoring::DetailedByCall,
        metric: "CPUUtilization",
        unit: "Percent",
        period: MINUTE,
        measurable: Measurable::GuestUp,
    },
    // "Status check metrics are available in 1-minute periods" under basic monitoring too, so
    // this row's period is a minute whichever state the instance is in.
    Row {
        case: "a status check of an instance under basic monitoring",
        monitoring: Monitoring::Basic,
        metric: "StatusCheckFailed_System",
        unit: "Count",
        period: MINUTE,
        measurable: Measurable::CheckVerdict,
    },
    Row {
        case: "a status check of an instance under detailed monitoring",
        monitoring: Monitoring::DetailedByCall,
        metric: "StatusCheckFailed_System",
        unit: "Count",
        period: MINUTE,
        measurable: Measurable::CheckVerdict,
    },
];

/// One row of the status-check table: what it covers, how the instance reached its monitoring
/// state and which check's series the row is of.
///
/// The three names are the status checks Amazon EC2 publishes a verdict of for one instance
/// (Amazon EC2 User Guide, "Status check metrics"), all three measured in `Count`; the
/// monitoring states span both cadences the instance could be publishing its GUEST measurements
/// at, which a status check's own minute period does not depend on.
const STATUS_CHECK_ROWS: &[(&str, Monitoring, &str)] = &[
    (
        "the system check of an instance under basic monitoring",
        Monitoring::Basic,
        "StatusCheckFailed_System",
    ),
    (
        "the instance check of an instance moved to detailed monitoring",
        Monitoring::DetailedByCall,
        "StatusCheckFailed_Instance",
    ),
    (
        "both checks at once, of an instance under basic monitoring",
        Monitoring::Basic,
        "StatusCheckFailed",
    ),
    (
        "the system check of an instance launched with detailed monitoring",
        Monitoring::DetailedAtLaunch,
        "StatusCheckFailed_System",
    ),
];

/// How many minutes of the sampler's clock the status-check case drives past the verdict: more
/// than one period, so the case sees the boundary the series BEGINS at and the boundaries after
/// it rather than only the first.
const VERDICT_SPAN_MINUTES: i64 = 3;

/// The unit every status-check metric is measured in: "the `StatusCheckFailed` metrics … 0
/// (passed) or 1 (failed)", a count of failed checks (Amazon EC2 User Guide, "Status check
/// metrics").
const CHECK_UNIT: &str = "Count";

/// The start of the minute `at` falls in, computed here rather than imported so the expected
/// grid is this suite's own.
fn minute_start(at: i64) -> i64 {
    at - at.rem_euclid(MINUTE)
}

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

/// Wall-clock now as the epoch second AT OR AFTER the instant it was read at: the ceiling of
/// that instant, where [`now_secs`] is its floor.
///
/// A reference instant the readings are compared against is bracketed from ABOVE, because a
/// reading is taken at a whole second: a reference second BELOW the instant it stands for would
/// make "the first reading is taken no later than one period past it" a statement about which
/// side of a second that instant fell on rather than about the instant.
fn now_secs_ceil() -> i64 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let whole = i64::try_from(since_epoch.as_secs()).unwrap_or_default();
    whole + i64::from(since_epoch.subsec_nanos() > 0)
}

/// The `(system status, instance status)` `DescribeInstanceStatus` reports for one instance, each
/// as its `SummaryStatus`, or empty strings for an instance the answer carries no status for.
async fn reported_check_statuses(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, String) {
    let body = ec2_ok(
        router,
        case,
        "DescribeInstanceStatus",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    let parsed = super::wire::document(case, &body);
    let Some(item) = parsed.descendants().find(|node| {
        node.is_element()
            && super::wire::child_text(*node, "instanceId").as_deref() == Some(instance_id)
    }) else {
        return (String::new(), String::new());
    };
    let summary_status = |name: &str| {
        super::wire::child(item, name)
            .and_then(|summary| super::wire::child_text(summary, "status"))
            .unwrap_or_default()
    };
    (
        summary_status("systemStatus"),
        summary_status("instanceStatus"),
    )
}

/// An upper bound, in epoch seconds, on the instant this row's series became MEASURABLE on the
/// instance it is of — the instant the first reading of it is owed at the next boundary past.
///
/// The condition is waited for through what the service itself reports, and the bound is read
/// once that report holds: a guest measurement needs the guest up, which [`running_instance`]
/// has already waited for, and a status check needs a verdict, which is polled for here. So the
/// reference instant is one the service has been seen to have reached rather than a span of the
/// wall clock anything is assumed to take.
async fn polled_measurable_by(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    measurable: Measurable,
) -> i64 {
    if measurable == Measurable::CheckVerdict {
        poll_until(
            case,
            "the instance's status checks to reach a verdict",
            || async move {
                let (system, instance) = reported_check_statuses(router, case, instance_id).await;
                CHECK_VERDICTS.contains(&system.as_str())
                    && CHECK_VERDICTS.contains(&instance.as_str())
            },
        )
        .await;
    }
    now_secs_ceil()
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
/// boundary of its own period after the series became measurable on it — within one period of
/// that, and never before the launch.
#[tokio::test]
async fn the_series_of_a_launched_instance_begins_after_it_came_up() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = emulator();
        let (instance_id, launched_at) = running_instance(&router, case, row.monitoring).await;
        // The instant the first reading is owed at the next boundary past: the guest up, and the
        // verdict reached for a series that is a status check's — each waited for through the
        // service's own reports.
        let measurable_by = polled_measurable_by(&router, case, &instance_id, row.measurable).await;

        let series = Series {
            router: &router,
            engine: &engine,
            case,
            instance_id: &instance_id,
            metric: row.metric,
            launched_at,
        };
        let (found, points) = series
            .first_reading(measurable_by, row.period + TICK_SLACK)
            .await;
        let found = found.unwrap_or_else(|| {
            panic!(
                "case {case:?}: a reading is taken at every boundary of this {}-second period, so \
                 one is owed within {} seconds of this series becoming measurable on the \
                 instance; after {} seconds of the sampler's clock the series held {points:?}",
                row.period,
                row.period,
                row.period + TICK_SLACK
            )
        });
        assert!(
            found <= row.period + TICK_SLACK,
            "case {case:?}: expected the first reading within {} seconds of this series becoming \
             measurable on the instance, got it after {found}",
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
            first.at <= measurable_by + row.period,
            "case {case:?}: the first reading is taken at the FIRST boundary after this series \
             became measurable on the instance, which it had by {measurable_by}, so it is stamped \
             no later than one {}-second period past that, got {first:?}",
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
    // The guest is up, which is what this series needs to be measurable on the instance.
    let measurable_by =
        polled_measurable_by(&router, case, &instance_id, Measurable::GuestUp).await;
    let series = Series {
        router: &router,
        engine: &engine,
        case,
        instance_id: &instance_id,
        metric: "CPUUtilization",
        launched_at,
    };
    let (found, while_running) = series
        .first_reading(measurable_by, MINUTE + TICK_SLACK)
        .await;
    let found = found.unwrap_or_else(|| {
        panic!("case {case:?}: the instance must publish while it runs, got {while_running:?}")
    });
    // The last instant the sampler was asked about while the instance was still RUNNING: every
    // reading of it was taken at or before that instant, since a reading is taken at a boundary
    // the sampler's clock has passed.
    let last_look_while_running = measurable_by + found;

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

/// Rule, for the cadence class whose reading is a VERDICT rather than a quantity: a status
/// check's datapoint carries the verdict that check had reached at the boundary it was taken at,
/// so the series of one begins at the first boundary at or after the check reported a verdict and
/// carries a reading at every boundary the sampler passes from there on.
///
/// Which boundary the guest's own coming up fell next to is therefore not what decides where the
/// series begins: a boundary between the boot and the verdict carries nothing, because a check
/// still initializing has no verdict to publish for the period it was looked at in. This case
/// pins that half of the window on its own, over the three checks Amazon EC2 publishes a verdict
/// of and under both monitoring cadences, with the reference instant read off the service's own
/// reports rather than off the wall clock.
#[tokio::test]
async fn a_status_check_series_begins_at_the_first_boundary_after_its_verdict() {
    for (case, monitoring, metric) in STATUS_CHECK_ROWS {
        let case = *case;
        let (router, engine) = emulator();
        let (instance_id, launched_at) = running_instance(&router, case, *monitoring).await;

        // A second the checks were seen with NO verdict at, read BEFORE the status it belongs
        // to, so it is at or before that look: no boundary at or before it may carry a reading
        // of this series. `None` when the verdict was already in at the first look, which is a
        // row with no such second to bound the series from below by.
        let before_the_look = now_secs();
        let (system, instance) = reported_check_statuses(&router, case, &instance_id).await;
        let still_initializing_at = (!CHECK_VERDICTS.contains(&system.as_str())
            || !CHECK_VERDICTS.contains(&instance.as_str()))
        .then_some(before_the_look);

        let verdict_by =
            polled_measurable_by(&router, case, &instance_id, Measurable::CheckVerdict).await;
        let series = Series {
            router: &router,
            engine: &engine,
            case,
            instance_id: &instance_id,
            metric,
            launched_at,
        };

        // The looks a running emulator takes, tick by tick past the verdict: the sampler's own
        // clock advanced rather than waited for, so several minute periods are covered without
        // spending minutes on them.
        let looks_until = verdict_by + VERDICT_SPAN_MINUTES * MINUTE;
        let mut look_at = verdict_by + SAMPLE_TICK;
        let mut last_look = verdict_by;
        while look_at <= looks_until {
            sample_at(&engine, look_at);
            last_look = look_at;
            look_at += SAMPLE_TICK;
        }

        let points = series.read().await;
        let first = points.first().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the check had a verdict by {verdict_by}, so the boundary after \
                 that carries a reading of {metric}; after {} seconds of the sampler's clock the \
                 series held nothing",
                VERDICT_SPAN_MINUTES * MINUTE
            )
        });
        assert!(
            first.at > launched_at,
            "case {case:?}: a reading is stamped with the instant it was taken, and nothing was \
             measured before the launch at {launched_at}, got {first:?}"
        );
        if let Some(initializing_at) = still_initializing_at {
            assert!(
                first.at > initializing_at,
                "case {case:?}: the check had reached no verdict at {initializing_at}, so no \
                 boundary at or before it carries a reading of {metric}, got {first:?} in \
                 {points:?}"
            );
        }
        assert!(
            first.at <= verdict_by + MINUTE,
            "case {case:?}: the series begins at the FIRST boundary after the verdict, which was \
             in by {verdict_by}, so it is stamped no later than one {MINUTE}-second period past \
             that, got {first:?}"
        );
        let stamps: Vec<i64> = points.iter().map(|point| point.at).collect();
        // Every minute boundary from the one the series begins at to the last the sampler was
        // asked past: a reading is taken at each of them, so the series has no gap in between.
        let expected: Vec<i64> = (first.at..=minute_start(last_look))
            .step_by(usize::try_from(MINUTE).unwrap_or(1))
            .collect();
        assert_eq!(
            stamps, expected,
            "case {case:?}: expected a reading of {metric} at every minute boundary from \
             {} through {}, got {points:?}",
            first.at,
            minute_start(last_look)
        );
        for point in &points {
            assert_eq!(
                point.unit, CHECK_UNIT,
                "case {case:?}: expected the unit {CHECK_UNIT:?} for {metric}, got {point:?}"
            );
        }
    }
}
