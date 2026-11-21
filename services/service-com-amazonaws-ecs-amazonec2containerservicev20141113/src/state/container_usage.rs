// SPDX-License-Identifier: CC0-1.0

//! The UTILIZATION figures Container Insights publishes for a cluster and for each of its
//! services: the CPU units and the MiB its running tasks are USING, beside what those tasks
//! RESERVE.
//!
//! # The rule this module implements
//!
//! The Container Insights namespace carries a cluster's utilization as absolute quantities
//! rather than as percentages: `CpuUtilized` and `CpuReserved` in CPU units (`TaskDefinition$cpu`:
//! "1024 CPU units is the equivalent of 1 vCPU") and `MemoryUtilized` and `MemoryReserved` in
//! MiB (`TaskDefinition$memory`), each for the tasks of one cluster and, beside the service
//! dimension, for the tasks of one service of it. What is USED is a measurement of those
//! tasks' containers; what is RESERVED is read off the task definitions they run.
//!
//! The measurement is the accounting each run keeps of what this emulator really performed
//! for it ([`super::container_consumption`]) — the same accounting the `AWS/ECS` percentages
//! are read off ([`super::service_utilization`]), so the two namespaces cannot report
//! different work for one container. This module is the second READING of it: a share of what
//! the containers were given is turned back into the quantity Container Insights states, by
//! multiplying it with what those tasks reserve.
//!
//! Each figure closes the INTERVAL since the same series last reported one, so the two
//! accountings a figure is the difference of are both that series' own
//! ([`InsightsBaseline`]). A series' first figure closes an interval of no length and reports
//! no CPU in use; the work its containers did is reported by the figure that does close one.
//!
//! # What is NOT reported
//!
//! A CPU figure needs the CPU the tasks were GIVEN to express itself against: the accounting
//! measures the share of its performers a program held, and the quantity that share stands for
//! is that share of what the task definitions reserve. So a cluster whose task definitions
//! reserve no CPU at all has no CPU quantity to report, and neither `CpuUtilized` nor
//! `CpuReserved` is published for it — as against a reservation of nothing, which would say
//! that nothing was given AND that nothing is used. The same holds for `MemoryReserved`. The
//! memory USED is measured directly, so it is reported whatever the definitions reserve.

use ::std::collections::{HashMap, HashSet};

use super::*;

/// How many bytes one mebibyte is, which is the unit Container Insights reports a memory
/// quantity in (`TaskDefinition$memory`).
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// What one Container Insights series reported LAST, so the next figure measures the interval
/// since then rather than the whole life of the runs behind it.
#[derive(Debug)]
pub struct InsightsBaseline {
    /// How long the containers of the series' tasks had spent executing when it last
    /// reported.
    executing_nanos: u128,
    /// When that was.
    measured_at: SystemTime,
}

/// The series one baseline belongs to: a cluster, and the service of it when the series is a
/// service's own.
pub type InsightsSeries = (String, Option<String>);

/// What one Container Insights series REPORTS: the quantities its tasks are using, and the
/// ones their task definitions reserve.
#[derive(Debug, Default, Clone, Copy)]
pub struct InsightsUtilization {
    /// The CPU units those tasks' containers used over the interval, and `None` for tasks
    /// whose definitions reserve no CPU to express that quantity against.
    pub cpu_utilized: Option<f64>,
    /// The CPU units those definitions reserve, under the same rule.
    pub cpu_reserved: Option<f64>,
    /// The MiB those containers are occupying — measured directly, so always reported.
    pub memory_utilized: f64,
    /// The MiB those definitions reserve, and `None` when they reserve none.
    pub memory_reserved: Option<f64>,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// What the running tasks of `cluster_name` are using and reserving, as of `now`.
    pub(super) fn cluster_insights_utilization(
        &mut self,
        cluster_name: &str,
        now: SystemTime,
    ) -> InsightsUtilization {
        let cluster = cluster_name.to_string();
        self.insights_utilization(
            (cluster.clone(), None),
            move |task| task.cluster_name == cluster,
            now,
        )
    }

    /// The same for the running tasks of ONE service of that cluster — the tasks its own
    /// scheduler placed, the ones `Service$runningCount` counts.
    pub(super) fn service_insights_utilization(
        &mut self,
        cluster_name: &str,
        service_name: &str,
        now: SystemTime,
    ) -> InsightsUtilization {
        let cluster = cluster_name.to_string();
        let service = service_name.to_string();
        self.insights_utilization(
            (cluster.clone(), Some(service.clone())),
            move |task| {
                task.cluster_name == cluster && task.service_name.as_deref() == Some(&service)
            },
            now,
        )
    }

    /// What the tasks `selected` picks are using and reserving over the interval since the
    /// series `series` last reported, with that series' baseline carried forward.
    fn insights_utilization(
        &mut self,
        series: InsightsSeries,
        selected: impl Fn(&TaskRecord) -> bool,
        now: SystemTime,
    ) -> InsightsUtilization {
        let reading = self.consumption_of(selected);
        let baseline = self
            .insights_baselines
            .entry(series)
            .or_insert_with(|| InsightsBaseline {
                executing_nanos: reading.executing_nanos,
                measured_at: now,
            });
        let span = now
            .duration_since(baseline.measured_at)
            .unwrap_or_default();
        let share = if span.is_zero() {
            // The interval has no length, so nothing can be reported as having been used over
            // it; the baseline stays open and the next figure closes a real interval.
            0.0
        } else {
            let share = reading.executing_share(baseline.executing_nanos, span);
            baseline.executing_nanos = reading.executing_nanos;
            baseline.measured_at = now;
            share
        };
        let reserved_cpu =
            (reading.reserved_cpu_units > 0).then_some(reading.reserved_cpu_units as f64);
        InsightsUtilization {
            // The share of what those tasks were given, as the quantity of CPU units that
            // share stands for.
            cpu_utilized: reserved_cpu.map(|reserved| share / 100.0 * reserved),
            cpu_reserved: reserved_cpu,
            memory_utilized: reading.held_bytes as f64 / BYTES_PER_MIB,
            memory_reserved: (reading.reserved_bytes > 0)
                .then(|| reading.reserved_bytes as f64 / BYTES_PER_MIB),
        }
    }

    /// Forget the baseline of every series that no longer exists: a cluster this store no
    /// longer holds, and a service of one it no longer holds, report nothing more, so a
    /// cluster or a service that is created again opens a series of its own.
    pub(super) fn prune_insights_baselines(&mut self) {
        let clusters = self
            .clusters
            .values()
            .map(|cluster| (cluster.name.clone(), None));
        let services = self
            .services
            .values()
            .filter(|service| service.is_active())
            .map(|service| (service.cluster_name.clone(), Some(service.name.clone())));
        let live: HashSet<InsightsSeries> = clusters.chain(services).collect();
        self.insights_baselines
            .retain(|series, _| live.contains(series));
    }
}

/// Where the baselines of the series this store publishes are held.
pub(super) type InsightsBaselines = HashMap<InsightsSeries, InsightsBaseline>;
