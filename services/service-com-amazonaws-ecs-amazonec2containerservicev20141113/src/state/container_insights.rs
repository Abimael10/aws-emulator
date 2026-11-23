// SPDX-License-Identifier: CC0-1.0

//! The CONTAINER INSIGHTS metrics a cluster publishes, and the cluster setting that turns
//! them on.
//!
//! # The rule this module implements
//!
//! `ClusterSetting$name` is `containerInsights` and `Cluster$settings` "indicates whether
//! CloudWatch Container Insights is on or off for a cluster": a cluster it is ON for
//! publishes the Container Insights metrics of its own namespace
//! ([`CONTAINER_INSIGHTS_NAMESPACE`]) into CloudWatch, and a cluster it is OFF for publishes
//! none at all — which is the whole observable difference between the two settings, and what
//! an alarm or a dashboard over a cluster reads.
//!
//! The datapoints go into the CloudWatch emulator's OWN metric store — the same store
//! `PutMetricData` writes and `GetMetricStatistics` / `GetMetricData` / `ListMetrics` read —
//! reached through the shared engine, so one metric has one home and the publishers cannot
//! drift (the Elastic Load Balancing v2 host counts take the same path).
//!
//! # What is published, and what is NOT
//!
//! Every datapoint is read off this store's own records, at the instant it is taken — the
//! COUNTS off the records themselves, and the UTILIZATION, the TRAFFIC and the STORAGE off the
//! runs this emulator performs for the containers of those records
//! ([`super::container_usage`]):
//!
//! * per cluster ([`CLUSTER_DIMENSION`]): how many of its tasks are running
//!   ([`METRIC_TASK_COUNT`]), how many services it holds ([`METRIC_SERVICE_COUNT`]) and how
//!   many container instances are registered into it
//!   ([`METRIC_CONTAINER_INSTANCE_COUNT`]), beside the CPU and memory its running tasks are
//!   using and reserving ([`METRIC_CPU_UTILIZED`], [`METRIC_CPU_RESERVED`],
//!   [`METRIC_MEMORY_UTILIZED`], [`METRIC_MEMORY_RESERVED`]), the bytes those tasks moved over
//!   a network and through a filesystem ([`METRIC_NETWORK_RX_BYTES`],
//!   [`METRIC_NETWORK_TX_BYTES`], [`METRIC_STORAGE_READ_BYTES`],
//!   [`METRIC_STORAGE_WRITE_BYTES`]) and the ephemeral storage their placements gave them
//!   beside what their layers hold ([`METRIC_EPHEMERAL_STORAGE_RESERVED`],
//!   [`METRIC_EPHEMERAL_STORAGE_UTILIZED`]);
//! * per service of that cluster ([`SERVICE_DIMENSION`] beside the cluster's): the
//!   running, pending and desired task counts of that service
//!   ([`METRIC_RUNNING_TASK_COUNT`], [`METRIC_PENDING_TASK_COUNT`],
//!   [`METRIC_DESIRED_TASK_COUNT`]), the deployments it holds
//!   ([`METRIC_DEPLOYMENT_COUNT`]), the task sets in it ([`METRIC_TASK_SET_COUNT`]) and the
//!   same utilization, traffic and storage figures for its own tasks;
//! * per TASK DEFINITION FAMILY whose tasks run in that cluster ([`FAMILY_DIMENSION`] beside
//!   the cluster's): those same figures for the tasks of that family, whatever placed them —
//!   the dimension pair a dashboard or an alarm over one workload selects.
//!
//! Each of those figures is a MEASUREMENT of the containers of those tasks: what this emulator
//! performs for a container is its program in this process, so the CPU figure is the CPU
//! units the programs of those containers have occupied, the memory figure the MiB their
//! sessions hold, the network figures the bytes their clients and their own servers really
//! carried and the storage figures the bytes their filesystem really read and wrote — never a
//! number for work nothing performed, and never a count of records this store merely holds.
//! Where each of them is measured is stated in [`super::container_consumption`].
//!
//! # The clock the series is published on
//!
//! Container Insights collects at a fixed interval, so a datapoint is owed to a cluster
//! whether or not anybody called: this is driven from the same pass that materialises a
//! task's own transitions and from the clock pump behind it
//! ([`super::settle_container_insights`]), and each cluster's series is published at most
//! once per [`PUBLISH_SPACING`]. Nothing here is on a read path, and nothing it publishes
//! depends on who called: what is written is the count the records already held.
//!
//! # The performance log events beside the series
//!
//! One collection also writes the PERFORMANCE LOG EVENTS of that cluster
//! ([`super::container_insights_logs`]), which is the other half of what a cluster Container
//! Insights is on for produces: the documents a consumer of the cluster's performance log
//! group reads, carrying the very figures this collection published.

use ::std::collections::BTreeMap;

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::*;

/// The namespace Amazon ECS publishes its Container Insights metrics into (CloudWatch User
/// Guide, "Amazon ECS Container Insights metrics").
pub const CONTAINER_INSIGHTS_NAMESPACE: &str = "ECS/ContainerInsights";

/// The dimension every Container Insights metric of a cluster carries: the cluster's own
/// name.
pub const CLUSTER_DIMENSION: &str = "ClusterName";

/// The dimension a SERVICE-level metric carries beside the cluster's: the service's own
/// name.
pub const SERVICE_DIMENSION: &str = "ServiceName";

/// The dimension a TASK-level metric carries beside the cluster's: the family of the task
/// definition its tasks run (`TaskDefinition$family`), which is how a dashboard or an alarm
/// selects the tasks of one workload whatever placed them.
pub const FAMILY_DIMENSION: &str = "TaskDefinitionFamily";

/// The cluster-level metrics this module publishes, by the names Container Insights
/// publishes them under.
pub const METRIC_TASK_COUNT: &str = "TaskCount";
pub const METRIC_SERVICE_COUNT: &str = "ServiceCount";
pub const METRIC_CONTAINER_INSTANCE_COUNT: &str = "ContainerInstanceCount";

/// The service-level metrics it publishes, by the same rule.
pub const METRIC_RUNNING_TASK_COUNT: &str = "RunningTaskCount";
pub const METRIC_PENDING_TASK_COUNT: &str = "PendingTaskCount";
pub const METRIC_DESIRED_TASK_COUNT: &str = "DesiredTaskCount";
pub const METRIC_DEPLOYMENT_COUNT: &str = "DeploymentCount";
pub const METRIC_TASK_SET_COUNT: &str = "TaskSetCount";

/// The UTILIZATION metrics it publishes at both levels: the CPU units and the MiB the
/// running tasks are using, beside what their task definitions reserve, by the names
/// Container Insights publishes them under.
pub const METRIC_CPU_UTILIZED: &str = "CpuUtilized";
pub const METRIC_CPU_RESERVED: &str = "CpuReserved";
pub const METRIC_MEMORY_UTILIZED: &str = "MemoryUtilized";
pub const METRIC_MEMORY_RESERVED: &str = "MemoryReserved";

/// The TRAFFIC metrics it publishes at both levels: the bytes those tasks' containers
/// received and sent over a network over the interval the datapoint closes
/// ([`super::container_usage`]).
pub const METRIC_NETWORK_RX_BYTES: &str = "NetworkRxBytes";
pub const METRIC_NETWORK_TX_BYTES: &str = "NetworkTxBytes";

/// The STORAGE metrics it publishes at both levels: the bytes they read from and wrote to a
/// filesystem over that same interval, and the ephemeral storage their placements gave them
/// beside what their own writable layers hold — the last two in the gigabytes that storage is
/// sized in ([`STORAGE_SIZE_UNIT`]).
pub const METRIC_STORAGE_READ_BYTES: &str = "StorageReadBytes";
pub const METRIC_STORAGE_WRITE_BYTES: &str = "StorageWriteBytes";
pub const METRIC_EPHEMERAL_STORAGE_RESERVED: &str = "EphemeralStorageReserved";
pub const METRIC_EPHEMERAL_STORAGE_UTILIZED: &str = "EphemeralStorageUtilized";

/// The `StandardUnit` a number of CPU UNITS is published in: a CPU unit is not one of the
/// units the enumerated type names, so these two carry the value it has for a measurement
/// with no unit of its own ([`cloudwatch::UNIT_NONE`]).
const CPU_UNIT: &str = cloudwatch::UNIT_NONE;

/// The `StandardUnit` an amount of memory is published in — the unit a task definition sizes
/// memory in.
const MEMORY_UNIT: &str = "Megabytes";

/// The `StandardUnit` a count of resources is published in.
const COUNT_UNIT: &str = "Count";

/// The `StandardUnit` a quantity of BYTES is published in — the unit the traffic and the
/// storage counters above are measured in.
const BYTES_UNIT: &str = "Bytes";

/// The `StandardUnit` an amount of EPHEMERAL STORAGE is published in: that storage is sized in
/// gibibytes (`TaskEphemeralStorage$sizeInGiB`), so both of its figures carry the unit of a
/// storage size rather than the one the byte counters beside them carry.
const STORAGE_SIZE_UNIT: &str = "Gigabytes";

/// The `StorageResolution` these datapoints are stored at, in seconds: the standard
/// resolution every metric a service publishes for itself takes ("standard resolution, with
/// data having a one-minute granularity", `PutMetricData`).
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// How long one cluster's series waits between datapoints.
///
/// EMULATOR-CHOSEN, and far shorter than the interval Container Insights collects on: a
/// client that turns the setting on and then watches the namespace would otherwise have to
/// sit through minutes before the first datapoint exists, while nothing about what is
/// published changes with the spacing — each datapoint is the count the records held when it
/// was taken.
pub const PUBLISH_SPACING: Duration = Duration::from_secs(1);

/// The `containerInsights` values that turn Container Insights ON for a cluster.
///
/// The setting's own catalog: "To use Container Insights with enhanced observability, set
/// the `containerInsights` account setting to `enhanced`. To use Container Insights, set the
/// `containerInsights` account setting to `enabled`" — so both of those publish, and
/// `disabled` publishes nothing.
const CONTAINER_INSIGHTS_ON: [&str; 2] = ["enabled", "enhanced"];

/// The `containerInsights` cluster setting, by the name the model gives it
/// (`ClusterSettingName`).
const CONTAINER_INSIGHTS_SETTING: &str = "containerInsights";

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The Container Insights datapoints every cluster this store holds owes as of `now`,
    /// and the instant each of those clusters next owes one.
    ///
    /// A cluster the setting is OFF for owes none, so nothing is published for it at all; one
    /// whose last collection is less than [`PUBLISH_SPACING`] old owes none YET, so a pass
    /// that runs twice in quick succession collects one series once — UNLESS its counts have
    /// moved since, which is a collection owed whatever the spacing: the series exists so a
    /// client can watch those counts, and a count that changed and changed back between two
    /// collections is a count the series never reported.
    fn due_container_insights(
        &mut self,
        now: SystemTime,
    ) -> DueInsights {
        // A series of a cluster or a service this store no longer holds reports nothing more,
        // so what it last reported is forgotten with it ([`super::container_usage`]).
        self.prune_insights_baselines();
        let cluster_names: Vec<String> = self
            .clusters
            .values()
            .filter(|cluster| self.container_insights_on(cluster))
            .filter(|cluster| {
                let spacing_elapsed = cluster
                    .insights_published_at
                    .is_none_or(|last| now.duration_since(last).unwrap_or_default() >= PUBLISH_SPACING);
                spacing_elapsed
                    || cluster.insights_published_tally != Some(self.cluster_insights_tally(&cluster.name))
            })
            .map(|cluster| cluster.name.clone())
            .collect();
        let mut published = Vec::new();
        let mut collected: Vec<ClusterPerformance> = Vec::new();
        for cluster_name in cluster_names {
            let tally = self.tally(&cluster_name);
            let instances = self.registered_container_instances(&cluster_name);
            let mut counts: Vec<(BTreeMap<String, String>, &str, f64, &str)> = Vec::new();
            // The cluster's OWN telemetry stream, which its cluster-level document and the
            // service-level ones beside it travel ([`super::container_insights_logs`]).
            let mut cluster_telemetry: Vec<(i64, String)> = Vec::new();
            let mut streams: Vec<PerformanceStream> = Vec::new();
            let stamped_at = super::container_insights_logs::epoch_millis(now);
            let cluster_dimensions: BTreeMap<String, String> =
                [(CLUSTER_DIMENSION.to_string(), cluster_name.clone())]
                    .into_iter()
                    .collect();
            let mut cluster_figures: Vec<(&str, f64, &str)> = [
                (METRIC_TASK_COUNT, tally.running as f64),
                (METRIC_SERVICE_COUNT, tally.active_services as f64),
                (METRIC_CONTAINER_INSTANCE_COUNT, instances as f64),
            ]
            .into_iter()
            .map(|(metric_name, count)| (metric_name, count, COUNT_UNIT))
            .collect();
            // What every running task of the cluster is consuming, moving and holding, read
            // off the accounting the runs of their containers keep
            // ([`super::container_usage`]).
            cluster_figures.extend(insights_figures(
                &self.cluster_insights_utilization(&cluster_name, now),
            ));
            // The same figures the series carries are what the cluster's own performance log
            // event carries ([`super::container_insights_logs`]).
            cluster_telemetry.push((
                stamped_at,
                performance_document(
                    CLUSTER_DOCUMENT_TYPE,
                    &cluster_dimensions,
                    &cluster_figures,
                    &[&[CLUSTER_DIMENSION]],
                    now,
                ),
            ));
            for (metric_name, figure, unit) in cluster_figures {
                counts.push((cluster_dimensions.clone(), metric_name, figure, unit));
            }
            let services: Vec<(String, i32, usize)> = self
                .services
                .values()
                .filter(|service| service.cluster_name == cluster_name && service.is_active())
                .map(|service| {
                    (
                        service.name.clone(),
                        service.desired_count,
                        // The deployment under way, and every earlier one whose tasks this
                        // one is still replacing ([`ServiceRecord::superseded_deployments`]).
                        1 + service.superseded_deployments.len(),
                    )
                })
                .collect();
            for (service_name, desired, deployments) in services {
                let tally = self.service_tally(&cluster_name, &service_name);
                let task_sets = self
                    .task_sets
                    .keys()
                    .filter(|(cluster, service, _)| {
                        *cluster == cluster_name && *service == service_name
                    })
                    .count();
                let dimensions: BTreeMap<String, String> = [
                    (CLUSTER_DIMENSION.to_string(), cluster_name.clone()),
                    (SERVICE_DIMENSION.to_string(), service_name.clone()),
                ]
                .into_iter()
                .collect();
                let mut service_figures: Vec<(&str, f64, &str)> = [
                    (METRIC_RUNNING_TASK_COUNT, f64::from(tally.running)),
                    (METRIC_PENDING_TASK_COUNT, f64::from(tally.pending)),
                    (METRIC_DESIRED_TASK_COUNT, f64::from(desired)),
                    (METRIC_DEPLOYMENT_COUNT, deployments as f64),
                    (METRIC_TASK_SET_COUNT, task_sets as f64),
                ]
                .into_iter()
                .map(|(metric_name, count)| (metric_name, count, COUNT_UNIT))
                .collect();
                // And what that service's own running tasks are consuming, on the dimension
                // pair a per-service alarm and dashboard select.
                service_figures.extend(insights_figures(&self.service_insights_utilization(
                    &cluster_name,
                    &service_name,
                    now,
                )));
                cluster_telemetry.push((
                    stamped_at,
                    performance_document(
                        SERVICE_DOCUMENT_TYPE,
                        &dimensions,
                        &service_figures,
                        &[&[CLUSTER_DIMENSION, SERVICE_DIMENSION]],
                        now,
                    ),
                ));
                for (metric_name, figure, unit) in service_figures {
                    counts.push((dimensions.clone(), metric_name, figure, unit));
                }
            }
            // And the TASK-level collection, on the dimension pair a dashboard over one
            // workload selects ([`FAMILY_DIMENSION`]): the same figures for the tasks of each
            // task definition family running in the cluster.
            for family in self.running_task_families(&cluster_name) {
                let dimensions: BTreeMap<String, String> = [
                    (CLUSTER_DIMENSION.to_string(), cluster_name.clone()),
                    (FAMILY_DIMENSION.to_string(), family.clone()),
                ]
                .into_iter()
                .collect();
                let family_figures =
                    insights_figures(&self.family_insights_utilization(&cluster_name, &family, now));
                for (metric_name, figure, unit) in family_figures {
                    counts.push((dimensions.clone(), metric_name, figure, unit));
                }
            }
            // One task's own telemetry is its own document, in the stream of that task: what
            // the containers of THAT task are using, moving and holding, which is the finest
            // thing this collection measures ([`super::container_insights_logs`]).
            for task in self.running_cluster_tasks(&cluster_name) {
                let mut identity: BTreeMap<String, String> = [
                    (CLUSTER_DIMENSION.to_string(), cluster_name.clone()),
                    (FAMILY_DIMENSION.to_string(), task.family.clone()),
                    (REVISION_MEMBER.to_string(), task.revision.clone()),
                    (TASK_ID_MEMBER.to_string(), task.id.clone()),
                    (KNOWN_STATUS_MEMBER.to_string(), task.known_status.clone()),
                ]
                .into_iter()
                .collect();
                // What the task carries only where its placement resolved one: the service
                // that placed it, and the launch type it was placed under.
                if let Some(service_name) = task.service_name.clone() {
                    identity.insert(SERVICE_DIMENSION.to_string(), service_name);
                }
                if let Some(launch_type) = task.launch_type.clone() {
                    identity.insert(LAUNCH_TYPE_MEMBER.to_string(), launch_type);
                }
                let task_figures =
                    insights_figures(&self.task_insights_utilization(&cluster_name, &task.arn, now));
                // The dimension sets a task's figures are published under: the cluster's own
                // series, the service's where a service placed the task, and the task
                // definition family's — the three a task's measurement feeds.
                let mut dimension_sets: Vec<&[&str]> = vec![
                    &[CLUSTER_DIMENSION],
                    &[CLUSTER_DIMENSION, FAMILY_DIMENSION],
                ];
                if task.service_name.is_some() {
                    dimension_sets.push(&[CLUSTER_DIMENSION, SERVICE_DIMENSION]);
                }
                let document = (
                    stamped_at,
                    performance_document(
                        TASK_DOCUMENT_TYPE,
                        &identity,
                        &task_figures,
                        &dimension_sets,
                        now,
                    ),
                );
                match task.container_instance_id {
                    // The stream the agent of the container instance this task runs on writes.
                    Some(instance_id) => streams.push(PerformanceStream {
                        log_stream: instance_id,
                        documents: vec![document],
                    }),
                    // A task Fargate runs holds no container instance of its own, so its
                    // telemetry travels a stream of the task.
                    None if task.fargate => streams.push(PerformanceStream {
                        log_stream: fargate_telemetry_stream(&task.id),
                        documents: vec![document],
                    }),
                    // A task this store holds no host for travels with the cluster's own
                    // collection rather than through a stream named after nothing.
                    None => cluster_telemetry.push(document),
                }
            }
            if let Some(cluster) = self.clusters.get_mut(&cluster_name) {
                cluster.insights_published_at = Some(now);
                cluster.insights_published_tally =
                    Some((tally.running, tally.active_services, instances));
            }
            let timestamp_secs = epoch_seconds(now);
            for (dimensions, metric_name, count, unit) in counts {
                published.push((
                    cloudwatch::MetricKey {
                        namespace: CONTAINER_INSIGHTS_NAMESPACE.to_string(),
                        metric_name: metric_name.to_string(),
                        dimensions,
                    },
                    cloudwatch::StoredDatum {
                        timestamp_secs,
                        // One observation: this collection's own reading of the records.
                        sample_count: 1.0,
                        sum: count,
                        minimum: count,
                        maximum: count,
                        unit: Some(unit.to_string()),
                        storage_resolution: STORAGE_RESOLUTION_SECS,
                    },
                ));
            }
            streams.push(PerformanceStream {
                log_stream: cluster_telemetry_stream(&cluster_name),
                documents: cluster_telemetry,
            });
            collected.push(ClusterPerformance {
                cluster_name,
                streams,
            });
        }
        DueInsights {
            datapoints: published,
            collected,
        }
    }

    /// The counts a collection of `cluster_name`'s CLUSTER-level metrics carries: its running
    /// tasks, its active services and the container instances registered into it — the three
    /// `DescribeClusters` reports, and what tells one collection from the one before it
    /// ([`Self::due_container_insights`]).
    fn cluster_insights_tally(&self, cluster_name: &str) -> (usize, usize, usize) {
        let tally = self.tally(cluster_name);
        (
            tally.running,
            tally.active_services,
            self.registered_container_instances(cluster_name),
        )
    }

    /// Whether Container Insights is ON for `cluster`: the value of its
    /// `containerInsights` setting as the cluster CARRIES it — the one its create named or
    /// captured and its own updates wrote — exactly as `Cluster$settings` reports it
    /// ([`Self::cluster_settings_view`]).
    fn container_insights_on(&self, cluster: &ClusterRecord) -> bool {
        self.cluster_settings_view(cluster)
            .iter()
            .filter(|setting| setting.name.as_deref() == Some(CONTAINER_INSIGHTS_SETTING))
            .any(|setting| {
                setting
                    .value
                    .as_deref()
                    .is_some_and(|value| CONTAINER_INSIGHTS_ON.contains(&value))
            })
    }
}

/// `at` as the epoch seconds a stored datapoint carries, which is the unit the metric
/// store's own readers window by. An instant before the epoch (which no clock reports) reads
/// as the epoch itself rather than failing the publish.
pub(super) fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(::std::time::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The utilization datapoints one reading carries, each with the unit it is published in: the
/// CPU units and the MiB the tasks are using, the ones their task definitions reserve, the
/// bytes those tasks moved over a network and through a filesystem, and the ephemeral storage
/// they were given beside what their layers hold.
///
/// A figure the reading has none of is not published at all — there is no quantity of CPU to
/// report for tasks that were given none, and no usage of an ephemeral storage no placement
/// gave them ([`super::container_usage`]). A byte COUNTER is published whatever it stands at:
/// zero bytes moved is a measurement of the traffic those containers had.
fn insights_figures(
    utilization: &InsightsUtilization,
) -> Vec<(&'static str, f64, &'static str)> {
    [
        (METRIC_CPU_UTILIZED, utilization.cpu_utilized, CPU_UNIT),
        (METRIC_CPU_RESERVED, utilization.cpu_reserved, CPU_UNIT),
        (
            METRIC_MEMORY_UTILIZED,
            Some(utilization.memory_utilized),
            MEMORY_UNIT,
        ),
        (
            METRIC_MEMORY_RESERVED,
            utilization.memory_reserved,
            MEMORY_UNIT,
        ),
        (
            METRIC_NETWORK_RX_BYTES,
            Some(utilization.network_received_bytes),
            BYTES_UNIT,
        ),
        (
            METRIC_NETWORK_TX_BYTES,
            Some(utilization.network_sent_bytes),
            BYTES_UNIT,
        ),
        (
            METRIC_STORAGE_READ_BYTES,
            Some(utilization.storage_read_bytes),
            BYTES_UNIT,
        ),
        (
            METRIC_STORAGE_WRITE_BYTES,
            Some(utilization.storage_written_bytes),
            BYTES_UNIT,
        ),
        (
            METRIC_EPHEMERAL_STORAGE_RESERVED,
            utilization.ephemeral_storage_reserved,
            STORAGE_SIZE_UNIT,
        ),
        (
            METRIC_EPHEMERAL_STORAGE_UTILIZED,
            utilization.ephemeral_storage_utilized,
            STORAGE_SIZE_UNIT,
        ),
    ]
    .into_iter()
    .filter_map(|(metric_name, figure, unit)| figure.map(|figure| (metric_name, figure, unit)))
    .collect()
}

/// What one collection pass OWES: the datapoints to publish into the metric store, and the
/// performance log events to write beside them, per cluster collected for
/// ([`super::container_insights_logs`]).
///
/// Both come out of ONE reading of this store's records, so a cluster's series and its
/// performance log event cannot report different figures for the same instant.
#[derive(Debug, Default)]
pub struct DueInsights {
    /// The datapoints, each with the series it belongs to.
    pub datapoints: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)>,
    /// What each cluster's performance log group is owed.
    pub collected: Vec<ClusterPerformance>,
}

/// Publish the Container Insights datapoints and performance log events the clock owes the
/// clusters this store holds — the ONE call the clock pump makes for it.
///
/// Called with NO guard of this store held: it takes this store's lock to read what is due
/// (and to stamp the clusters it read), releases it, and only then writes the CloudWatch
/// store and the log group — the one-directional, guard-free discipline every other
/// cross-store access in this crate follows, which is what keeps a cycle between the stores
/// unreachable.
///
/// Nothing is published on an engine that holds no metric store to reach: a datapoint has
/// one home, and this path never makes a second one.
pub async fn settle_container_insights<Eng: Send + Sync + 'static>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    engine: &Eng,
    region: &str,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The guard this statement takes is a TEMPORARY: it is released at the end of the
    // statement, so nothing below holds it.
    let due = state.lock().due_container_insights(SystemTime::now());
    if due.datapoints.is_empty() && due.collected.is_empty() {
        return;
    }    publish_insights_datapoints(engine, due.datapoints);
    // The other half of what a collection produces: the documents a consumer of the cluster's
    // performance log group reads ([`super::container_insights_logs`]). Written with no guard
    // of either store held, because the write goes through CloudWatch Logs' own ingest path.
    super::container_insights_logs::deliver_performance_logs(callback, region, &due.collected)
        .await;
}

/// Append `published` to the CloudWatch metric store the shared engine holds, and make the
/// alarms over those series due.
fn publish_insights_datapoints<Eng: Send + Sync + 'static>(
    engine: &Eng,
    published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)>,
) {
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
            // The metric store holds no more series. The records the count was read off are
            // already stored, so nothing is failed for it; the dropped datapoint is reported
            // where a developer can see it.
            ::tracing::warn!(
                metric = watched.metric_name.as_str(),
                "the CloudWatch metric store is at capacity, so this Container Insights \
                 datapoint was dropped"
            );
            continue;
        }
        written.push(cloudwatch::WrittenSeries {
            key: watched,
            units: vec![unit],
        });
    }
    // An alarm is evaluated FROM these datapoints, so storing a series it watches is what
    // makes its evaluation due — an alarm cannot tell a service's own metric from one a
    // client published.
    store.make_alarms_due(&written, epoch_seconds(now));
}
