// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a group's metrics collection publishes, and the clock they are
//! published on.
//!
//! # The rule this module implements
//!
//! Enabling group metrics collection is not a flag a group merely reports back: "Enables
//! group metrics collection for the specified Auto Scaling group. You can use these metrics
//! to track changes in an Auto Scaling group and to set alarms on threshold values. You can
//! view group metrics using the Amazon EC2 Auto Scaling console or the CloudWatch console."
//! (`EnableMetricsCollection`) — so a group that collects a metric AGGREGATES it into
//! CloudWatch, in the namespace this service owns ([`METRIC_NAMESPACE`]), dimensioned by the
//! group's own name ([`GROUP_NAME_DIMENSION`]), at the granularity the enable asked for
//! ("The frequency at which Amazon EC2 Auto Scaling sends aggregated data to CloudWatch. The
//! only valid value is 1Minute", `EnableMetricsCollectionQuery$Granularity`, which is
//! [`PUBLISH_INTERVAL`]). A client watches its groups through those series: a dashboard, a
//! CloudWatch alarm over a group metric and any capacity-based target tracking read them and
//! nothing else, so a group that publishes none leaves every one of them with no data at all.
//!
//! WHICH series exist is the group's own collection and nothing else
//! ([`super::metrics_collection`]): one series per collected metric, so a metric the group
//! does not collect is a series nobody published — which is what a read of it, and
//! `ListMetrics` over this namespace, finds.
//!
//! WHAT each series reads is computed from the group's own state at the instant the datapoint
//! is published, by [`metric_value`] — the one definition of every metric of this service's
//! catalog ([`super::METRIC_COLLECTION_TYPES`]), so no two readers of one metric can
//! disagree. The two FAMILIES of that catalog differ in their unit, which is the whole of the
//! weighting rule: an INSTANCE metric counts instances, while a CAPACITY metric counts the
//! capacity units those instances contribute — "you can specify the number of capacity units
//! provided by each instance type … the capacity units count toward the desired capacity"
//! (`LaunchTemplateOverrides$WeightedCapacity`), the same weights the group's own size
//! parameters are stated in ([`super::StoredGroup::instance_capacity_units`]). So for a group
//! whose instance type is weighted at two units, the in-service CAPACITY reads twice the
//! in-service INSTANCE count, and the desired capacity it is converging on is in those units
//! too; for an unweighted group every instance is worth one unit and the two families read
//! alike.
//!
//! A datapoint is published with no unit of its own: the quantities are bare counts, which
//! the `StandardUnit` vocabulary has no member for, so CloudWatch reports them under the
//! value that type declares for a measurement that has none.
//!
//! # The clock the series are published on
//!
//! A group publishes one datapoint per metric per [`PUBLISH_INTERVAL`], which is what the
//! granularity it is collected at means, and every datapoint is stamped on a WHOLE minute
//! ([`whole_minute`]) — the instant CloudWatch aggregates a regular-resolution datapoint
//! published inside that minute at — so the metrics of one group share their minute buckets
//! whenever each of them was enabled. A client reading a series that has just been enabled
//! would otherwise have a whole minute to sit through before it held anything, so a series
//! OPENS with its run-up already behind it: [`RUN_UP_PERIODS`] datapoints come due
//! [`ACCELERATED_SPACING`] apart (the emulator's own span, not an AWS one) while each keeps
//! the one-minute SPACING the real series has, and from the instant the series has caught up
//! with the present it publishes one datapoint per interval. A reader therefore finds a
//! series that has been running, and an alarm created straight after the enable has periods
//! to grade.
//!
//! Nothing here decides WHEN a datapoint is published: the stored due instant does, and
//! [`AutoScalingState::publish_due_group_metrics`] materialises exactly the datapoints that
//! instant has already made due — the same pass every operation makes
//! ([`AutoScalingState::materialize_due`]), and the one the clock pump makes with no further
//! call from anybody ([`super::drive_lifecycle_clock`]), which is what a client polling
//! CloudWatch alone needs. So what a caller sees does not depend on whether anybody called.
//!
//! The datapoints go into the CloudWatch emulator's OWN metric store — the store
//! `PutMetricData` writes and `ListMetrics`, `GetMetricStatistics` and `GetMetricData` read —
//! reached through the shared engine, so one metric has one home. They are appended to it
//! directly rather than through `PutMetricData`, which reserves the `AWS/` namespaces for the
//! services that own them ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricData$Namespace`).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::{
    AutoScalingState, StoredGroup, StoredInstance, LIFECYCLE_DETACHED, LIFECYCLE_DETACHING,
    LIFECYCLE_IN_SERVICE, LIFECYCLE_PENDING, LIFECYCLE_TERMINATING, WARMED_PENDING,
    WARMED_TERMINATING,
};

/// The CloudWatch namespace Amazon EC2 Auto Scaling publishes a group's own metrics into.
///
/// The namespace of the service that owns them, which is the one a client reads them from.
pub const METRIC_NAMESPACE: &str = "AWS/AutoScaling";

/// The dimension naming the group an observation is about: its `AutoScalingGroupName`.
pub const GROUP_NAME_DIMENSION: &str = "AutoScalingGroupName";

/// How far apart the datapoints of one series are STAMPED.
///
/// The granularity a group aggregates at: "The frequency at which Amazon EC2 Auto Scaling
/// sends aggregated data to CloudWatch. The only valid value is 1Minute."
/// (`EnableMetricsCollectionQuery$Granularity`, the sole entry of
/// [`super::METRIC_GRANULARITIES`]).
pub const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);

/// The `StorageResolution` these datapoints are stored at, in seconds: the regular resolution
/// a metric has unless its publisher asks for the high-resolution one ("This field is
/// optional, if you do not specify it the default of 60 is used",
/// `MetricDatum$StorageResolution`), which is the minute these are aggregated per.
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// How many datapoints a series OPENS with, stamped one [`PUBLISH_INTERVAL`] apart ahead of
/// the instant the group began collecting the metric.
///
/// The emulator's own figure, not an AWS one: the real series has been running for as long as
/// the collection has, so a reader — or an alarm created straight after the enable — sees as
/// many periods of data as it asks for. Five periods is the run-up of the longest evaluation
/// an `EvaluationPeriods` of one to five asks for.
const RUN_UP_PERIODS: u32 = 5;

/// How far apart the datapoints of one series come DUE while the series is still catching up
/// with the present.
///
/// The emulator's own span: the run-up is published in about a second, so a caller reading the
/// series the instant the enable was served still finds the empty series a group that has
/// published nothing yet has.
const ACCELERATED_SPACING: Duration = Duration::from_millis(200);

/// How many datapoints ONE pass publishes, over every series this store holds.
///
/// A bound on the work one pass does, not a quota: a group this pass has no room for keeps the
/// due instant it carries, so the next pass publishes its minute intact rather than one pass
/// doing unbounded work. One group's whole minute is at most one datapoint per metric of the
/// catalog, which [`super::METRIC_COLLECTION_TYPES`] bounds far below this cap, so no group
/// can be too big for a pass of its own.
const MAX_DATAPOINTS_PER_PASS: usize = 2_000;

/// One series' place in its schedule: the stamp its next datapoint carries, and when that
/// datapoint comes due.
///
/// ONE schedule per metric a group collects, so a metric added to a collection that already
/// has one begins publishing at once rather than waiting for the minute the group was already
/// on. The schedules still LINE UP, because every stamp is a whole minute
/// ([`whole_minute`]) and advances by exactly one [`PUBLISH_INTERVAL`] per publish, however
/// long the publish itself waited: so all the metrics of one group fall in the same minute
/// buckets whenever each of them was enabled, and a reader finds the whole family at one time
/// stamp — which is what makes "the capacity is twice the instance count" readable at a
/// single instant.
#[derive(Debug)]
struct SeriesClock {
    /// The time stamp the next datapoint carries. Advances by [`PUBLISH_INTERVAL`] per publish,
    /// so the series keeps the spacing the real one has.
    next_stamp: SystemTime,
    /// The instant that datapoint comes due; see [`ACCELERATED_SPACING`].
    next_due_at: SystemTime,
}

/// Where every group-metric series of this store has got to.
///
/// Held on the store so the schedule is the store's own: a schedule exists while the group
/// collects the metric it belongs to, and is dropped with the group or when a disable takes
/// that metric out of its collection.
#[derive(Debug, Default)]
pub struct GroupMetricPublisher {
    /// The schedules of each group that is collecting, keyed by its `AutoScalingGroupName` and
    /// then by the metric's own name, so one group's schedules are reached together and a pass
    /// reads each group once.
    series: BTreeMap<String, BTreeMap<String, SeriesClock>>,
}

/// The series one metric of one group is published into.
///
/// THE ONE place a published series is addressed, so the publish and every reader that looks
/// for what it stored name the same series.
#[must_use]
pub fn series_key(group_name: &str, metric: &str) -> cloudwatch::MetricKey {
    cloudwatch::MetricKey {
        namespace: METRIC_NAMESPACE.to_string(),
        metric_name: metric.to_string(),
        dimensions: [(
            GROUP_NAME_DIMENSION.to_string(),
            group_name.to_string(),
        )]
        .into_iter()
        .collect(),
    }
}

/// What the metric `metric` of `group` reads at the instant it is published, or `None` when
/// `metric` names no metric of this service's catalog.
///
/// THE ONE definition of every metric of [`super::METRIC_COLLECTION_TYPES`], computed from the
/// group's own state: the size parameters it was configured with, the lifecycle state of each
/// of its members, and the capacity units each member contributes
/// ([`StoredGroup::instance_capacity_units`], the same weights the group's size parameters are
/// stated in and its members report). So a group metric and the group's own reads cannot
/// disagree, and no value here is kept anywhere a second time.
///
/// The two families, from the metrics' own documented semantics: a `*Instances` metric counts
/// INSTANCES in one lifecycle state, and its `*Capacity` twin counts the capacity units those
/// same instances contribute. The warm pool's metrics are the same two families over the
/// group's warm pool ([`super::warm_pools`]), and the two `GroupAndWarmPool*` metrics are the
/// group's own total and the pool's added together.
#[must_use]
pub fn metric_value(group: &StoredGroup, metric: &str) -> Option<f64> {
    let value = match metric {
        // The three size parameters the group was configured with, in the units its capacity
        // is counted in.
        "GroupMinSize" => f64::from(group.min_size),
        "GroupMaxSize" => f64::from(group.max_size),
        "GroupDesiredCapacity" => f64::from(group.desired_capacity),

        // One family per lifecycle state: the instances in it, and the capacity they provide.
        "GroupInServiceInstances" => instances(group, is_in_service),
        "GroupInServiceCapacity" => capacity(group, is_in_service),
        "GroupPendingInstances" => instances(group, is_pending),
        "GroupPendingCapacity" => capacity(group, is_pending),
        "GroupStandbyInstances" => instances(group, is_standby),
        "GroupStandbyCapacity" => capacity(group, is_standby),
        "GroupTerminatingInstances" => instances(group, is_terminating),
        "GroupTerminatingCapacity" => capacity(group, is_terminating),
        "GroupTerminatingRetainedInstances" => instances(group, is_terminating_retained),
        "GroupTerminatingRetainedCapacity" => capacity(group, is_terminating_retained),
        "GroupTotalInstances" => instances(group, is_member),
        "GroupTotalCapacity" => capacity(group, is_member),

        // The warm pool's own minimum and the size it maintains against the group's capacity,
        // then the same families over the instances it holds. Zero for a group with no warm
        // pool, which holds no warmed instance and maintains none.
        "WarmPoolMinSize" => f64::from(group.warm_pool().map_or(0, |pool| pool.min_size)),
        "WarmPoolDesiredCapacity" => f64::from(group.warm_pool_target_size()),
        "WarmPoolWarmedCapacity" => warm_pool_capacity(group, is_warmed),
        "WarmPoolPendingCapacity" => warm_pool_capacity(group, is_warmed_pending),
        "WarmPoolPendingRetainedCapacity" => warm_pool_capacity(group, is_warmed_pending_retained),
        "WarmPoolTerminatingCapacity" => warm_pool_capacity(group, is_warmed_terminating),
        "WarmPoolTerminatingRetainedCapacity" => {
            warm_pool_capacity(group, is_warmed_terminating_retained)
        }
        "WarmPoolTotalCapacity" => warm_pool_capacity(group, is_any_instance),

        // The group and its warm pool taken together, each the sum of the two the pair is
        // read off, so a reader cannot find the sum disagreeing with its parts.
        "GroupAndWarmPoolDesiredCapacity" => {
            f64::from(group.desired_capacity) + f64::from(group.warm_pool_target_size())
        }
        "GroupAndWarmPoolTotalCapacity" => {
            capacity(group, is_member) + warm_pool_capacity(group, is_any_instance)
        }

        _ => return None,
    };
    Some(value)
}

/// How many of this group's own members `counted` holds for.
fn instances(group: &StoredGroup, counted: fn(&StoredInstance) -> bool) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let counted = group.instances.iter().filter(|held| counted(held)).count() as f64;
    counted
}

/// The capacity units those same members contribute.
fn capacity(group: &StoredGroup, counted: fn(&StoredInstance) -> bool) -> f64 {
    group
        .instances
        .iter()
        .filter(|held| counted(held))
        .map(|held| f64::from(group.instance_capacity_units(held)))
        .sum()
}

/// The capacity units the members of this group's WARM POOL that `counted` holds for
/// contribute. Zero for a group with no warm pool, which holds no warmed instance.
fn warm_pool_capacity(group: &StoredGroup, counted: fn(&StoredInstance) -> bool) -> f64 {
    group
        .warm_pool_instances()
        .iter()
        .filter(|held| counted(held))
        .map(|held| f64::from(group.instance_capacity_units(held)))
        .sum()
}

/// Whether this member is in service.
fn is_in_service(instance: &StoredInstance) -> bool {
    instance.lifecycle_state == LIFECYCLE_IN_SERVICE
}

/// Whether this member is still coming into service — including one a launch lifecycle hook is
/// holding, which is pending until the hold resolves.
fn is_pending(instance: &StoredInstance) -> bool {
    instance.lifecycle_state.starts_with(LIFECYCLE_PENDING)
}

/// Whether this member is parked out of service, or on its way there
/// ([`StoredInstance::is_in_standby`]).
fn is_standby(instance: &StoredInstance) -> bool {
    instance.is_in_standby()
}

/// Whether this member is on its way out of the group by being terminated — including one a
/// termination hook is holding, and excluding one the group RETAINED, which has its own pair of
/// metrics below.
fn is_terminating(instance: &StoredInstance) -> bool {
    instance.lifecycle_state.starts_with(LIFECYCLE_TERMINATING) && !instance.is_retained()
}

/// Whether this member is one the group retained instead of terminating
/// ([`StoredInstance::is_retained`]).
fn is_terminating_retained(instance: &StoredInstance) -> bool {
    instance.is_retained()
}

/// Whether this member is still ONE OF THE GROUP's, which is what the TOTAL family counts:
/// "the total number of instances in the Auto Scaling group" is every instance the group
/// holds, whatever lifecycle state it is in — in service, coming into service, parked out of
/// service, on its way out, or retained instead of terminated. Only an instance that has left
/// the group WITHOUT being terminated is none of its own: `DetachInstances` "removes one or
/// more instances from the specified Auto Scaling group. After the instances are detached, you
/// can manage them independent of the Auto Scaling group", so from then on the group does not
/// hold it.
///
/// The total is therefore the sum of the families above it, which is what keeps a reader from
/// finding a total that disagrees with its parts.
fn is_member(instance: &StoredInstance) -> bool {
    instance.lifecycle_state != LIFECYCLE_DETACHING
        && instance.lifecycle_state != LIFECYCLE_DETACHED
}

/// Whether this warm pool member is still being prepared.
fn is_warmed_pending(instance: &StoredInstance) -> bool {
    instance.lifecycle_state.starts_with(WARMED_PENDING)
}

/// Whether this warm pool member is on its way out of the pool.
fn is_warmed_terminating(instance: &StoredInstance) -> bool {
    instance.lifecycle_state.starts_with(WARMED_TERMINATING)
}

/// Whether this warm pool member has been prepared: it is neither coming into the pool nor
/// leaving it, so it is sitting in the pool's own state waiting to be used.
fn is_warmed(instance: &StoredInstance) -> bool {
    !is_warmed_pending(instance) && !is_warmed_terminating(instance)
}

/// Whether this warm pool member is being prepared AND retained — the pair of retained warm
/// pool metrics counts the instances an instance maintenance policy keeps in the pool across a
/// replacement, a state nothing in this service puts a warmed instance in, so they read the
/// nothing the pool holds of them rather than a figure of their own.
fn is_warmed_pending_retained(instance: &StoredInstance) -> bool {
    is_warmed_pending(instance) && instance.is_retained()
}

/// Whether this warm pool member is leaving the pool AND retained; see
/// [`is_warmed_pending_retained`].
fn is_warmed_terminating_retained(instance: &StoredInstance) -> bool {
    is_warmed_terminating(instance) && instance.is_retained()
}

/// Every member, for the total of a collection.
fn is_any_instance(_instance: &StoredInstance) -> bool {
    true
}

impl AutoScalingState {
    /// Publish every group-metric datapoint the clock has already made due at `now`, into the
    /// account's own CloudWatch metric store.
    ///
    /// Part of the pass every operation makes ([`Self::materialize_due`]) and the one the
    /// clock pump makes on its own, because it changes WHEN nothing: a minute whose due
    /// instant has not arrived publishes nothing however often this is called, and a minute
    /// that is due was due whether or not anybody called.
    pub fn publish_due_group_metrics<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        now: SystemTime,
    ) {
        self.sync_group_metric_series(now);
        let published = self.due_group_metric_datapoints(now);
        append_datapoints(engine, published, now);
    }

    /// Whether this store owes a group-metric datapoint at `now` — what keeps the clock pump
    /// awake while any group is collecting.
    ///
    /// A metric a group has just begun collecting has no schedule yet, and a schedule opens
    /// with its first datapoint already due, so it is owed at once. Read over the metrics the
    /// service's own catalog defines — the ones a schedule is opened for — so nothing that
    /// cannot be published is ever reported as owed.
    pub(crate) fn owes_group_metrics(&self, now: SystemTime) -> bool {
        self.groups.values().any(|group| {
            let schedules = self.group_metrics.series.get(&group.name);
            group
                .enabled_metrics
                .keys()
                .filter(|metric| super::is_valid_metric_collection_type(metric))
                .any(|metric| {
                    schedules
                        .and_then(|schedules| schedules.get(metric))
                        .is_none_or(|clock| clock.next_due_at <= now)
                })
        })
    }

    /// The instants the group-metric schedules of this store are waiting on, for the pump that
    /// waits for the earliest instant anything is owed at.
    pub(crate) fn group_metric_due_instants(&self) -> impl Iterator<Item = SystemTime> + '_ {
        self.group_metrics
            .series
            .values()
            .flat_map(BTreeMap::values)
            .map(|clock| clock.next_due_at)
    }

    /// Open a schedule for every metric every group collects, and drop the schedule of every
    /// series whose group this store no longer holds or whose metric is no longer collected.
    ///
    /// A schedule opens with its run-up already behind it ([`RUN_UP_PERIODS`]) and its first
    /// datapoint due at once, so the pass that finds the collection publishes it itself — a
    /// metric added to a collection that already has schedules begins publishing straight away
    /// rather than waiting for the minute the group was already on. A schedule that already
    /// exists keeps the place it has reached, so a collection nothing changed keeps ONE series
    /// per metric rather than restarting its run-up.
    ///
    /// ONLY a metric of this service's own catalog gets one
    /// ([`super::METRIC_COLLECTION_TYPES`], which is what the writers of a collection accept),
    /// so nothing is ever scheduled that has no value to publish.
    fn sync_group_metric_series(&mut self, now: SystemTime) {
        let Self {
            groups,
            group_metrics,
            ..
        } = self;
        // The series a collection no longer names, dropped without allocating anything: a
        // store whose collections did not change does this pass and nothing else.
        group_metrics.series.retain(|group_name, schedules| {
            let Some(group) = groups.get(group_name) else {
                return false;
            };
            schedules.retain(|metric, _| group.enabled_metrics.contains_key(metric));
            !schedules.is_empty()
        });
        let run_up = PUBLISH_INTERVAL.saturating_mul(RUN_UP_PERIODS);
        // Every stamp is a whole minute, which is what keeps the schedules of one group lined
        // up however far apart the enables that opened them were.
        let opens_at = whole_minute(now);
        for group in groups.values() {
            let collected: Vec<&String> = group
                .enabled_metrics
                .keys()
                .filter(|metric| super::is_valid_metric_collection_type(metric))
                .collect();
            if collected.is_empty() {
                continue;
            }
            let schedules = group_metrics.series.entry(group.name.clone()).or_default();
            for metric in collected {
                if !schedules.contains_key(metric) {
                    schedules.insert(
                        metric.clone(),
                        SeriesClock {
                            next_stamp: opens_at.checked_sub(run_up).unwrap_or(opens_at),
                            next_due_at: now,
                        },
                    );
                }
            }
        }
    }

    /// Take the datapoints every schedule that is due at `now` publishes, and advance each of
    /// those schedules to its next minute.
    ///
    /// The due minute of ONE group is taken together or not at all: a pass with no room for all
    /// of that group's due metrics leaves their schedules where they are — the minute is still
    /// due, and the next pass publishes it whole ([`MAX_DATAPOINTS_PER_PASS`]). The group is
    /// read ONCE for all of them, which is also what makes every metric of one minute report
    /// the same observation of the group.
    ///
    /// Every schedule the pass TAKES is advanced, whether or not it had a value to publish, so
    /// a pass always makes progress and no schedule can stay due for ever.
    fn due_group_metric_datapoints(
        &mut self,
        now: SystemTime,
    ) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
        let Self {
            groups,
            group_metrics,
            ..
        } = self;
        let mut published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> = Vec::new();
        for (group_name, schedules) in &mut group_metrics.series {
            let Some(group) = groups.get(group_name) else {
                continue;
            };
            let due: Vec<String> = schedules
                .iter()
                .filter(|(_, clock)| clock.next_due_at <= now)
                .map(|(metric, _)| metric.clone())
                .collect();
            // Room for this group's WHOLE minute, taken before any of its schedules moves.
            if published.len() + due.len() > MAX_DATAPOINTS_PER_PASS {
                return published;
            }
            for metric in due {
                let Some(clock) = schedules.get_mut(&metric) else {
                    continue;
                };
                // The schedule moves on whatever this minute reads, so a pass always makes
                // progress. A name this service's catalog does not define has no value to
                // publish and is never scheduled ([`Self::sync_group_metric_series`]); one
                // that reached here all the same publishes nothing and keeps its place in the
                // schedule like every other.
                let stamp = advance(clock, now);
                if let Some(value) = metric_value(group, &metric) {
                    published.push((series_key(group_name, &metric), datum(stamp, value)));
                }
            }
        }
        published
    }
}

/// `now` rounded DOWN to the whole minute it falls in, which is the instant CloudWatch
/// aggregates a regular-resolution datapoint published inside that minute at.
///
/// Stamping the lattice on whole minutes is what makes two series of one group share their
/// minute buckets whenever each of them was enabled: every stamp is a multiple of
/// [`PUBLISH_INTERVAL`] from the epoch, and a publish advances a schedule by exactly one
/// interval however long the publish itself waited.
fn whole_minute(now: SystemTime) -> SystemTime {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let minutes = since.as_secs() - since.as_secs() % PUBLISH_INTERVAL.as_secs();
    UNIX_EPOCH
        .checked_add(Duration::from_secs(minutes))
        .unwrap_or(now)
}

/// Take the time stamp of the minute that is due and schedule the next one, `now` being what
/// the clock read when this pass started.
fn advance(clock: &mut SeriesClock, now: SystemTime) -> SystemTime {
    let stamp = clock.next_stamp;
    clock.next_stamp = stamp.checked_add(PUBLISH_INTERVAL).unwrap_or(stamp);
    // While the series is still behind the present it is published at the accelerated spacing;
    // once it has caught up, the next datapoint is due when the clock reaches its stamp, which
    // is one publish interval away. Never longer than one interval: that is the most the
    // schedule can owe, so a clock that has stepped backwards costs one slow publish rather
    // than an unbounded wait.
    let wait = clock
        .next_stamp
        .duration_since(now)
        .unwrap_or(ACCELERATED_SPACING)
        .min(PUBLISH_INTERVAL);
    clock.next_due_at = now.checked_add(wait).unwrap_or(now);
    stamp
}

/// One observation of one metric, stamped `at`.
///
/// A group metric is a single reading of the group rather than an aggregate of several, so the
/// datum stands for one observation and its sum, minimum and maximum are that reading. It
/// declares no unit of its own: the reading is a bare count of instances or of capacity units,
/// which the `StandardUnit` vocabulary has no member for, and the metric store reports a
/// datapoint published without one under the value that enumerated type has for a measurement
/// that has none.
fn datum(at: SystemTime, value: f64) -> cloudwatch::StoredDatum {
    cloudwatch::StoredDatum {
        timestamp_secs: epoch_secs(at),
        sample_count: 1.0,
        sum: value,
        minimum: value,
        maximum: value,
        unit: None,
        storage_resolution: STORAGE_RESOLUTION_SECS,
    }
}

/// Append the datapoints this pass published to the CloudWatch metric store, and make every
/// alarm over the series they belong to due for evaluation.
///
/// Nothing is published on an engine that holds no metric store to reach: a datapoint has one
/// home, and this path never makes a second one. An alarm is evaluated FROM these datapoints,
/// so storing a series it watches is what makes its evaluation due — an alarm cannot tell a
/// service's own metric from one a client published, which is what makes a CloudWatch alarm
/// over a group metric sound.
fn append_datapoints<Eng: Send + Sync + 'static>(
    engine: &Eng,
    published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)>,
    now: SystemTime,
) {
    if published.is_empty() {
        return;
    }
    let Some(state) = cloudwatch::service_state(engine) else {
        return;
    };
    let mut store = state.lock();
    let mut written: Vec<cloudwatch::WrittenSeries> = Vec::with_capacity(published.len());
    for (key, datum) in published {
        let watched = key.clone();
        let unit = datum.unit().to_string();
        if store.append_datapoints(key, vec![datum]).is_err() {
            // The metric store holds no more series. The group itself is unaffected, so
            // nothing is failed for it; the dropped observation is reported where a developer
            // can see it.
            ::tracing::warn!(
                metric = watched.metric_name.as_str(),
                "the CloudWatch metric store is at capacity, so this group metric was dropped"
            );
            continue;
        }
        written.push(cloudwatch::WrittenSeries {
            key: watched,
            units: vec![unit],
        });
    }
    store.make_alarms_due(&written, epoch_secs(now));
}

/// `at` as the epoch seconds a stored datapoint carries, which is the unit the metric store's
/// own readers window by. An instant before the epoch (which no clock reports) reads as the
/// epoch itself rather than failing the publish.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}
