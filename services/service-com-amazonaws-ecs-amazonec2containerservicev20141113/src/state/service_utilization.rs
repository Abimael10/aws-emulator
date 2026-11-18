// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ECS` UTILIZATION series a SERVICE publishes while it holds a running task.
//!
//! # The rule this module implements
//!
//! Amazon ECS publishes the CPU and the memory a service's tasks use as the `AWS/ECS` metrics
//! [`METRIC_CPU_UTILIZATION`] and [`METRIC_MEMORY_UTILIZATION`] — each a PERCENTAGE
//! ([`PERCENT_UNIT`]) of what the service's task definition reserves, which is why neither is
//! ever negative and neither passes 100 — dimensioned on exactly the service they measure: the
//! pair [`CLUSTER_DIMENSION`] + [`SERVICE_DIMENSION`] (the ECS developer guide's "Service
//! utilization", whose formula divides what the tasks of one service use by what that
//! service's task definition reserves). That pair is the whole series identity: it is what a
//! per-service alarm and an Application Auto Scaling target-tracking policy select, so a
//! service that publishes nothing leaves every such alarm without data.
//!
//! A SERVICE-level series therefore exists for a service and never for a cluster alone. The
//! cluster-wide utilization of the same namespace is a different measurement — it divides what
//! the cluster's tasks use by what its registered CONTAINER INSTANCES offer — and nothing here
//! measures a container instance's consumption, so no `ClusterName`-only series is published
//! at all: a cluster running Fargate tasks has no container instance to measure, and an alarm
//! on that dimension alone receives no data, which is what tells the two dimension sets apart.
//!
//! The series is the base namespace's, so it has nothing to do with the `containerInsights`
//! cluster setting: that setting decides whether the cluster publishes the Container Insights
//! namespace beside this one ([`super::container_insights`]), and a service of a cluster it is
//! off for publishes these two metrics all the same.
//!
//! The datapoints go into the CloudWatch emulator's OWN metric store — the same store
//! `PutMetricData` writes and `GetMetricStatistics` / `GetMetricData` / `ListMetrics` read —
//! reached through the shared engine, so one metric has one home and the publishers cannot
//! drift (the Container Insights counts beside it and the Elastic Load Balancing v2 host
//! counts take the same path).
//!
//! # What one datapoint SAYS
//!
//! Each of these two numbers is a measurement taken on the containers of a running task, and
//! what this emulator performs for a container is its program inside this process
//! ([`super::container_shell`]) with no resource accounting of any kind standing behind it — so
//! in every period the quantity each metric measures is zero: nothing is using the CPU units
//! the task reserved, and nothing is occupying the MiB it reserved either. Any other number
//! would be a measurement of work nothing measured, which a client could not tell from a
//! measured one ([`UTILIZATION_SAMPLE`]). What the series does carry is what this store DOES
//! know: which services are being metered, from when, on which dimension pair, in which unit
//! and at which resolution.
//!
//! # When a datapoint is published
//!
//! Only while the service HOLDS a running task: the metrics measure the tasks of the service,
//! so a service whose tasks have not started yet, one that wants none, and one that has been
//! deleted are measured by nothing and publish nothing — which is what makes the arrival of a
//! series the observable fact that a service is running.
//!
//! A service's datapoints are stamped one [`PUBLISH_INTERVAL`] apart, the cadence Amazon ECS
//! reports these at and the finest period an alarm over a regular-resolution metric can be
//! evaluated at. A series OPENS with [`RUN_UP_PERIODS`] periods already behind it, published
//! [`ACCELERATED_SPACING`] apart until its stamps reach the present and one publish interval
//! apart from then on: the real series has been running for as long as the tasks have, so an
//! alarm created straight after the service converged sees as many periods of data as its
//! `EvaluationPeriods` asks for instead of waiting minutes for them. Those two spans are the
//! emulator's own, and they change the SCHEDULE alone — never what a datapoint says.
//!
//! The collection is owed by the clock, whether or not anybody calls: it is driven from the
//! same pass that materialises a task's own transitions and from the clock pump behind it
//! ([`settle_service_utilization`]), which
//! [`ComAmazonawsEcsAmazonec2containerservicev20141113State::next_service_utilization_due_at`]
//! keeps awake while a service is being metered. Nothing here is on a read path, and nothing
//! it publishes depends on who called.

use ::std::collections::{BTreeMap, HashSet};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::container_insights::epoch_seconds;
use super::*;

/// The namespace Amazon ECS publishes a service's own utilization into — the BASE namespace of
/// the service, as against the Container Insights one
/// ([`super::container_insights::CONTAINER_INSIGHTS_NAMESPACE`]).
pub const SERVICE_METRIC_NAMESPACE: &str = "AWS/ECS";

/// The percentage of the CPU units the service's task definition reserves that its tasks are
/// using (the ECS developer guide's service `CPUUtilization`).
pub const METRIC_CPU_UTILIZATION: &str = "CPUUtilization";

/// The percentage of the memory the service's task definition reserves — the task's hard
/// memory limit — that its tasks are using (service `MemoryUtilization`).
pub const METRIC_MEMORY_UTILIZATION: &str = "MemoryUtilization";

/// The `StandardUnit` a percentage is published in.
pub const PERCENT_UNIT: &str = "Percent";

/// What every sample of both metrics measured: nothing, so zero.
///
/// Each of them is a measurement taken on the containers of a running task — service
/// `CPUUtilization` is the CPU units those containers use over the CPU units the task
/// definition reserves, and service `MemoryUtilization` the memory they occupy over the memory
/// it reserves — and what this emulator performs for a container is its program inside this
/// process, with no resource accounting behind it at all. So in every period the quantity each
/// metric measures is zero, which is also what each of their definitions gives for exactly
/// that period. Any other number would be a measurement of work nothing measured, which a
/// client could not tell from a measured one.
pub const UTILIZATION_SAMPLE: f64 = 0.0;

/// The `StorageResolution` these datapoints are stored at, in seconds: the standard resolution
/// every metric a service publishes about itself takes ("standard resolution, with data having
/// a one-minute granularity", `PutMetricData`).
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// How far apart the datapoints of one series are STAMPED: Amazon ECS reports a service's
/// utilization per minute, which is also the finest period an alarm over a regular-resolution
/// metric can be evaluated at.
pub const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);

/// How many periods a series OPENS with, stamped one [`PUBLISH_INTERVAL`] apart ahead of the
/// instant the service began being metered.
///
/// THE EMULATOR'S OWN FIGURE, not an AWS one: the real series has been running for as long as
/// the service's tasks have, so an alarm created straight after the service converged sees as
/// many periods of data as its `EvaluationPeriods` asks for. Five periods is the run-up the
/// longest evaluation an `EvaluationPeriods` of one to five asks for needs.
const RUN_UP_PERIODS: u32 = 5;

/// How far apart the DATAPOINTS of one series come due while that series is still catching up
/// with the present.
///
/// THE EMULATOR'S OWN SPAN, and the publish schedule's alone: it never changes what a datapoint
/// says, and it is deliberately non-zero, so a caller that reads the instant a service starts
/// running still finds the series opening rather than complete.
const ACCELERATED_SPACING: Duration = Duration::from_millis(250);

/// One service's place in its publish schedule: the stamp its next datapoint carries, and when
/// that datapoint comes due.
#[derive(Debug)]
pub struct UtilizationSeries {
    /// The time stamp the next datapoint carries. Advances by [`PUBLISH_INTERVAL`] per
    /// datapoint, so the series keeps the spacing the real one has.
    next_stamp: SystemTime,
    /// The instant the next datapoint comes due. Stored, never slept on.
    next_datapoint_at: SystemTime,
}

impl UtilizationSeries {
    /// Take the time stamp of the datapoint that is due, and schedule the next one.
    ///
    /// While the series is still behind the present the next datapoint is due one
    /// [`ACCELERATED_SPACING`] later; once its stamps have caught up, it is due when the clock
    /// reaches the stamp it carries. Never longer than one publish interval — that is the most
    /// the schedule can owe, so a clock that stepped backwards costs one slow datapoint rather
    /// than an unbounded wait — and never shorter than the accelerated spacing, so a catch-up
    /// cannot become a spin.
    fn advance(&mut self, now: SystemTime) -> SystemTime {
        let stamp = self.next_stamp;
        self.next_stamp = stamp.checked_add(PUBLISH_INTERVAL).unwrap_or(stamp);
        let wait = self
            .next_stamp
            .duration_since(now)
            .unwrap_or(ACCELERATED_SPACING)
            .clamp(ACCELERATED_SPACING, PUBLISH_INTERVAL);
        self.next_datapoint_at = now.checked_add(wait).unwrap_or(now);
        stamp
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Every service this store is METERING: the `(cluster, service)` pair of each ACTIVE
    /// service that holds at least one running task.
    ///
    /// The tasks are what the two metrics measure, so a service holding none is measured by
    /// nothing — read off the very task records `DescribeServices` reports `runningCount` from
    /// ([`Self::service_tally`]), so the services that publish are exactly the ones a client
    /// reads as running.
    fn metered_services(&self) -> HashSet<(String, String)> {
        self.services
            .values()
            .filter(|service| service.is_active())
            .filter(|service| self.service_tally(&service.cluster_name, &service.name).running > 0)
            .map(|service| (service.cluster_name.clone(), service.name.clone()))
            .collect()
    }

    /// The `AWS/ECS` datapoints the services this store holds owe as of `now`, with each
    /// metered service's series advanced to its next datapoint.
    ///
    /// A service that is no longer metered has its series CLOSED, so a service that stopped
    /// publishes nothing more and one that runs again opens a new series with its own run-up.
    fn due_service_utilization(
        &mut self,
        now: SystemTime,
    ) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
        let metered = self.metered_services();
        self.service_utilization
            .retain(|service, _| metered.contains(service));
        let run_up = PUBLISH_INTERVAL.saturating_mul(RUN_UP_PERIODS);
        for service in metered {
            self.service_utilization
                .entry(service)
                .or_insert_with(|| UtilizationSeries {
                    next_stamp: now.checked_sub(run_up).unwrap_or(now),
                    // The first datapoint of a series is due at once: the service has been
                    // running for as long as its tasks have.
                    next_datapoint_at: now,
                });
        }
        let mut published = Vec::new();
        for ((cluster_name, service_name), series) in &mut self.service_utilization {
            if series.next_datapoint_at > now {
                continue;
            }
            let stamp = series.advance(now);
            let dimensions: BTreeMap<String, String> = [
                (CLUSTER_DIMENSION.to_string(), cluster_name.clone()),
                (SERVICE_DIMENSION.to_string(), service_name.clone()),
            ]
            .into_iter()
            .collect();
            let timestamp_secs = epoch_seconds(stamp);
            for metric_name in [METRIC_CPU_UTILIZATION, METRIC_MEMORY_UTILIZATION] {
                published.push((
                    cloudwatch::MetricKey {
                        namespace: SERVICE_METRIC_NAMESPACE.to_string(),
                        metric_name: metric_name.to_string(),
                        dimensions: dimensions.clone(),
                    },
                    cloudwatch::StoredDatum {
                        timestamp_secs,
                        // One observation: this collection's own reading of the service.
                        sample_count: 1.0,
                        sum: UTILIZATION_SAMPLE,
                        minimum: UTILIZATION_SAMPLE,
                        maximum: UTILIZATION_SAMPLE,
                        unit: Some(PERCENT_UNIT.to_string()),
                        storage_resolution: STORAGE_RESOLUTION_SECS,
                    },
                ));
            }
        }
        published
    }

    /// When the next datapoint any series this store is publishing comes due, if any.
    ///
    /// What keeps the clock pump awake while a service is being metered: a running service's
    /// series is published for as long as it runs, and no further call is made between one
    /// datapoint and the next ([`super::task_events::drive_task_clock`]).
    pub(super) fn next_service_utilization_due_at(&self) -> Option<SystemTime> {
        self.service_utilization
            .values()
            .map(|series| series.next_datapoint_at)
            .min()
    }
}

/// Publish the `AWS/ECS` utilization datapoints the clock owes the services this store holds —
/// the ONE call the clock pump makes for it.
///
/// Called with NO guard of this store held: it takes this store's lock to read what is due (and
/// to advance the series it read), releases it, and only then writes the CloudWatch store — the
/// one-directional, guard-free discipline every other cross-store access in this crate follows,
/// which is what keeps a cycle between the stores unreachable.
///
/// Nothing is published on an engine that holds no metric store to reach: a datapoint has one
/// home, and this path never makes a second one.
pub fn settle_service_utilization<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The guard this statement takes is a TEMPORARY: it is released at the end of the
    // statement, so nothing below holds it.
    let published = state.lock().due_service_utilization(SystemTime::now());
    if published.is_empty() {
        return;
    }
    let now = SystemTime::now();
    let Some(metrics) = cloudwatch::service_state(engine) else {
        return;
    };
    let mut store = metrics.lock();
    let mut written: Vec<cloudwatch::WrittenSeries> = Vec::with_capacity(published.len());
    for (key, datum) in published {
        let watched = key.clone();
        let unit = datum.unit().to_string();
        if store.append_datapoints(key, vec![datum]).is_err() {
            // The metric store holds no more series. The records the reading was taken off are
            // already stored, so nothing is failed for it; the dropped datapoint is reported
            // where a developer can see it.
            ::tracing::warn!(
                metric = watched.metric_name.as_str(),
                "the CloudWatch metric store is at capacity, so this service utilization \
                 datapoint was dropped"
            );
            continue;
        }
        written.push(cloudwatch::WrittenSeries {
            key: watched,
            units: vec![unit],
        });
    }
    // An alarm is evaluated FROM these datapoints, so storing a series it watches is what makes
    // its evaluation due — an alarm cannot tell a service's own metric from one a client
    // published.
    store.make_alarms_due(&written, epoch_seconds(now));
}
