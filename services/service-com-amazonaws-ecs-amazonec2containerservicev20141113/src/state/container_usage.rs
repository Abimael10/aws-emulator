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
//! MiB (`TaskDefinition$memory`), each for the tasks of one cluster and, beside the service or
//! the task definition family dimension, for the tasks of one service of it or of one family in
//! it. What is USED is a measurement of those tasks' containers; what is RESERVED is read off
//! the task definitions they run.
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
//!
//! # The traffic and the storage of those tasks
//!
//! The same reading carries the four BYTE COUNTERS Container Insights publishes beside the two
//! above — `NetworkRxBytes` and `NetworkTxBytes`, `StorageReadBytes` and `StorageWriteBytes` —
//! each the bytes the containers of those tasks really moved over the interval the figure
//! closes: the requests their clients and their own servers carried
//! ([`super::container_http_client`], [`super::container_servers`]) and the objects their AWS
//! CLI transferred ([`super::container_aws_cli`]) for the network, and the reads and writes
//! their filesystem performed ([`super::container_filesystem`]) for the storage. A counter is
//! a measurement whatever it stands at, so a figure of zero is published for tasks that moved
//! nothing: that is what their traffic was.
//!
//! Their EPHEMERAL STORAGE is the pair of quantities the task's own placement and its layers
//! state, both in the GIGABYTES that storage is sized in rather than in bytes:
//! `EphemeralStorageReserved` is the storage the placement gave those tasks
//! (`TaskEphemeralStorage$sizeInGiB`, "the total amount, in GiB, of the ephemeral storage to
//! set for the task", resolved by [`super::resolved_ephemeral_storage_gib`]), and
//! `EphemeralStorageUtilized` what their containers' own writable layers hold at the instant it
//! is taken, in that same unit ([`super::container_filesystem`]). Neither is published for
//! tasks whose placement gave them no ephemeral storage to report at all — there is no amount
//! of storage to express a usage against, exactly as there is no CPU quantity for tasks given
//! no CPU.

use ::std::collections::{HashMap, HashSet};

use super::*;

/// How many bytes one mebibyte is, which is the unit Container Insights reports a memory
/// quantity in (`TaskDefinition$memory`).
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// How many bytes one gibibyte is, which is the unit a task's ephemeral storage is sized in
/// (`TaskEphemeralStorage$sizeInGiB`) and the one Container Insights reports both ephemeral
/// storage figures in.
const BYTES_PER_GIB: f64 = 1024.0 * BYTES_PER_MIB;

/// What one Container Insights series reported LAST, so the next figure measures the interval
/// since then rather than the whole life of the runs behind it.
#[derive(Debug)]
pub struct InsightsBaseline {
    /// How long the containers of the series' tasks had spent executing when it last
    /// reported.
    executing_nanos: u128,
    /// When that was.
    measured_at: SystemTime,
    /// What the four byte counters of those containers stood at when it last reported, so the
    /// next figure of each is the bytes moved since ([`ByteCounters`]).
    counters: ByteCounters,
}

/// The four cumulative BYTE COUNTERS one Container Insights series reports a difference of:
/// what the containers of its tasks have received and sent over a network, and read from and
/// written to a filesystem ([`super::container_consumption`]).
#[derive(Debug, Default, Clone, Copy)]
struct ByteCounters {
    network_received: u128,
    network_sent: u128,
    storage_read: u128,
    storage_written: u128,
}

impl ByteCounters {
    /// The counters one reading of those containers stands at.
    fn of(reading: &ServiceConsumption) -> Self {
        Self {
            network_received: reading.network_received_bytes,
            network_sent: reading.network_sent_bytes,
            storage_read: reading.storage_read_bytes,
            storage_written: reading.storage_written_bytes,
        }
    }

    /// The bytes moved between `self` and the LATER reading `now`, one difference per counter.
    ///
    /// Saturating: a series whose tasks changed under it (a task that stopped, one that was
    /// replaced) can read lower than it did, and bytes nothing moved are not reported as moved.
    fn moved_since(self, now: Self) -> Self {
        Self {
            network_received: now.network_received.saturating_sub(self.network_received),
            network_sent: now.network_sent.saturating_sub(self.network_sent),
            storage_read: now.storage_read.saturating_sub(self.storage_read),
            storage_written: now.storage_written.saturating_sub(self.storage_written),
        }
    }
}

/// The series one baseline belongs to: a cluster, and WHAT of that cluster the figures are of
/// ([`InsightsScope`]).
pub type InsightsSeries = (String, InsightsScope);

/// What of one cluster a Container Insights series reports on — the three things Container
/// Insights dimensions these figures by: the whole cluster, one SERVICE of it, and the tasks of
/// one TASK DEFINITION FAMILY in it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InsightsScope {
    /// Every running task of the cluster.
    Cluster,
    /// The running tasks of one service of it.
    Service(String),
    /// The running tasks of one task definition family in it.
    Family(String),
    /// ONE task of it, by its own ARN.
    Task(String),
}

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
    /// The bytes those containers RECEIVED over a network over the interval — a measurement,
    /// so zero where they received none.
    pub network_received_bytes: f64,
    /// The bytes they SENT over one, under the same rule.
    pub network_sent_bytes: f64,
    /// The bytes they READ from a filesystem over the interval, under the same rule.
    pub storage_read_bytes: f64,
    /// The bytes they WROTE to one, under the same rule.
    pub storage_written_bytes: f64,
    /// The GIGABYTES of EPHEMERAL STORAGE the placements of those tasks gave them, and `None`
    /// for tasks whose placement gave them none to report.
    pub ephemeral_storage_reserved: Option<f64>,
    /// What their containers' own writable layers hold, in that same unit, and `None` under the
    /// same rule as the reservation above: there is no usage of a storage nothing was given.
    pub ephemeral_storage_utilized: Option<f64>,
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
            (cluster.clone(), InsightsScope::Cluster),
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
            (cluster.clone(), InsightsScope::Service(service.clone())),
            move |task| {
                task.cluster_name == cluster && task.service_name.as_deref() == Some(&service)
            },
            now,
        )
    }

    /// The same for the running tasks of one TASK DEFINITION FAMILY in that cluster — whatever
    /// placed them, so a family's figures cover the tasks a service maintains and the
    /// standalone ones beside them.
    pub(super) fn family_insights_utilization(
        &mut self,
        cluster_name: &str,
        family: &str,
        now: SystemTime,
    ) -> InsightsUtilization {
        let cluster = cluster_name.to_string();
        let wanted = family.to_string();
        self.insights_utilization(
            (cluster.clone(), InsightsScope::Family(wanted.clone())),
            move |task| task.cluster_name == cluster && task.family == wanted,
            now,
        )
    }

    /// The same for ONE task of that cluster, by its own ARN — the figures its own telemetry
    /// carries ([`super::container_insights_logs`]).
    pub(super) fn task_insights_utilization(
        &mut self,
        cluster_name: &str,
        task_arn: &str,
        now: SystemTime,
    ) -> InsightsUtilization {
        let cluster = cluster_name.to_string();
        let wanted = task_arn.to_string();
        self.insights_utilization(
            (cluster, InsightsScope::Task(wanted.clone())),
            move |task| task.task_arn == wanted,
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
        let counters = ByteCounters::of(&reading);
        let baseline = self
            .insights_baselines
            .entry(series)
            .or_insert_with(|| InsightsBaseline {
                executing_nanos: reading.executing_nanos,
                measured_at: now,
                counters,
            });
        // The bytes moved since this series last reported, with the counters carried forward:
        // a figure closes the same interval the CPU figure beside it does.
        let moved = baseline.counters.moved_since(counters);
        baseline.counters = counters;
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
        // The ephemeral storage the placements of those tasks gave them, which is what their
        // layers' usage is reported beside; tasks given none have neither figure.
        let reserved_storage =
            (reading.reserved_storage_bytes > 0).then_some(reading.reserved_storage_bytes as f64);
        InsightsUtilization {
            // The share of what those tasks were given, as the quantity of CPU units that
            // share stands for.
            cpu_utilized: reserved_cpu.map(|reserved| share / 100.0 * reserved),
            cpu_reserved: reserved_cpu,
            memory_utilized: reading.held_bytes as f64 / BYTES_PER_MIB,
            memory_reserved: (reading.reserved_bytes > 0)
                .then(|| reading.reserved_bytes as f64 / BYTES_PER_MIB),
            network_received_bytes: moved.network_received as f64,
            network_sent_bytes: moved.network_sent as f64,
            storage_read_bytes: moved.storage_read as f64,
            storage_written_bytes: moved.storage_written as f64,
            ephemeral_storage_reserved: reserved_storage.map(|reserved| reserved / BYTES_PER_GIB),
            ephemeral_storage_utilized: reserved_storage
                .map(|_| reading.layer_bytes as f64 / BYTES_PER_GIB),
        }
    }

    /// Forget the baseline of every series that no longer exists: a cluster this store no
    /// longer holds, a service of one it no longer holds, and a task definition family no task
    /// of the cluster runs any more, report nothing more — so a cluster, a service or a family
    /// that runs again opens a series of its own.
    pub(super) fn prune_insights_baselines(&mut self) {
        let clusters = self
            .clusters
            .values()
            .map(|cluster| (cluster.name.clone(), InsightsScope::Cluster));
        let services = self
            .services
            .values()
            .filter(|service| service.is_active())
            .map(|service| {
                (
                    service.cluster_name.clone(),
                    InsightsScope::Service(service.name.clone()),
                )
            });
        let families = self.tasks.values().map(|task| {
            (
                task.cluster_name.clone(),
                InsightsScope::Family(task.family.clone()),
            )
        });
        let tasks = self.tasks.values().map(|task| {
            (
                task.cluster_name.clone(),
                InsightsScope::Task(task.task_arn.clone()),
            )
        });
        let live: HashSet<InsightsSeries> = clusters
            .chain(services)
            .chain(families)
            .chain(tasks)
            .collect();
        self.insights_baselines
            .retain(|series, _| live.contains(series));
    }

    /// The task definition FAMILIES whose tasks are running in `cluster_name`, in name order —
    /// the families a collection of that cluster reports figures for.
    pub(super) fn running_task_families(&self, cluster_name: &str) -> Vec<String> {
        let mut families: Vec<String> = self
            .running_cluster_tasks(cluster_name)
            .into_iter()
            .map(|task| task.family)
            .collect();
        families.sort();
        families.dedup();
        families
    }

    /// The RUNNING tasks of `cluster_name`, in ARN order, each as what one task's own telemetry
    /// is identified by ([`TaskTelemetry`]).
    pub(super) fn running_cluster_tasks(&self, cluster_name: &str) -> Vec<TaskTelemetry> {
        let mut running: Vec<TaskTelemetry> = self
            .tasks
            .values()
            .filter(|task| {
                task.cluster_name == cluster_name
                    && task.desired_status == STATUS_RUNNING
                    && task.last_status == STATUS_RUNNING
            })
            .map(|task| TaskTelemetry {
                id: last_segment(&task.task_arn).to_string(),
                arn: task.task_arn.clone(),
                family: task.family.clone(),
                // `TaskDefinition$revision`, as the revision the task runs carries it: the
                // last segment of the definition's own ARN (`family:revision`).
                revision: task
                    .task_definition_arn
                    .rsplit(':')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                service_name: task.service_name.clone(),
                launch_type: task.launch_type.clone(),
                known_status: task.last_status.clone(),
                container_instance_id: task
                    .container_instance_arn
                    .as_deref()
                    .map(|arn| last_segment(arn).to_string()),
                // A task Fargate runs is one whose placement gave it ephemeral storage of its
                // own ([`super::task_platform`]), which is the same reading its
                // ephemeral-storage figures are published under.
                fargate: task.ephemeral_storage_gib.is_some(),
            })
            .collect();
        running.sort_by(|left, right| left.arn.cmp(&right.arn));
        running
    }
}

/// ONE running task, as its own telemetry names it ([`super::container_insights_logs`]).
#[derive(Debug, Clone)]
pub struct TaskTelemetry {
    /// The task's own ARN, which its series is keyed by ([`InsightsScope::Task`]).
    pub arn: String,
    /// Its id: the last segment of that ARN.
    pub id: String,
    /// The family of the task definition it runs, and that definition's revision.
    pub family: String,
    pub revision: String,
    /// The service that placed it, and `None` for a standalone task.
    pub service_name: Option<String>,
    /// `Task$launchType`, and `None` for a placement that resolved none.
    pub launch_type: Option<String>,
    /// `Task$lastStatus`, which for a task being collected for is that it is running.
    pub known_status: String,
    /// The id of the container instance it runs on, and `None` for a task this store holds no
    /// host for — which is every task Fargate runs.
    pub container_instance_id: Option<String>,
    /// Whether Fargate runs it.
    pub fargate: bool,
}

/// The last `/`-separated segment of `arn`, which is how the resource an ARN names is
/// identified on its own.
fn last_segment(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// Where the baselines of the series this store publishes are held.
pub(super) type InsightsBaselines = HashMap<InsightsSeries, InsightsBaseline>;
