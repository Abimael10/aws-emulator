// SPDX-License-Identifier: CC0-1.0

//! The `CapacityProviderReservation` series Amazon ECS PUBLISHES for a capacity provider whose
//! managed scaling it runs — the series the target-tracking policy of
//! [`super::managed_scaling_policy`] holds at that provider's `targetCapacity`.
//!
//! # The rule this module implements
//!
//! `ManagedScaling`'s own documentation: "Amazon ECS manages a target tracking scaling policy
//! using an Amazon ECS managed CloudWatch metric with the specified `targetCapacity` value as
//! the target value for the metric." A target-tracking policy tracks a SERIES, and that series
//! is Amazon ECS's own — the service that owns the policy is the service that publishes the
//! metric — so a provider whose scaling Amazon ECS manages is a provider Amazon ECS publishes a
//! reservation for, per `(capacity provider, cluster)` pair, under the two dimensions the policy
//! selects it by ([`CAPACITY_PROVIDER_NAME_DIMENSION`], [`CLUSTER_NAME_DIMENSION`]). A policy
//! over a series nobody publishes drives nothing and leaves every alarm it owns without data,
//! which is the whole observable difference this module makes.
//!
//! What ONE datapoint SAYS is the reservation: how much of the capacity behind the provider the
//! cluster's work has taken, as a percentage of the capacity the provider's Auto Scaling group
//! holds — the Amazon ECS developer guide's cluster-auto-scaling ratio, the instances the work
//! needs over the instances the group holds, times a hundred ([`ReservationReading`]):
//!
//! * the instances the work NEEDS are the group's own instances the cluster has placed work on,
//!   plus one for each task of the pair still parked waiting for capacity to be acquired — the
//!   very shortfall [`super::managed_capacity_scaling`] reads as the instances to add, so the
//!   number that makes the series climb is the number that makes the group grow;
//! * the instances the group HOLDS are the members Amazon EC2 Auto Scaling reports for it,
//!   whatever state they are in on their way into service, which is what keeps the series from
//!   asking again for capacity that is already coming.
//!
//! Two readings a ratio cannot express are stated on their own: a group that holds nothing and
//! is asked for nothing is in BALANCE, so the series reads [`BALANCED_RESERVATION`] rather than
//! nothing at all, and a group that holds nothing while work waits reads
//! [`UNMET_CAPACITY_RESERVATION`], which stands above every target a provider may be given, so
//! a group at zero capacity scales out whatever `targetCapacity` it carries.
//!
//! # When a datapoint is published
//!
//! Only for a pair that HAS both halves of the series: a provider whose managed scaling is off,
//! one that is not over an Auto Scaling group, one that reserves capacity for no cluster and one
//! whose group Amazon EC2 Auto Scaling does not hold are each measured by nothing and publish
//! nothing — the same reading that decides whether the pair has a policy at all
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::managed_scaling_policies`]), so
//! the series a policy tracks exists exactly while that policy does.
//!
//! A pair's datapoints are stamped one [`PUBLISH_INTERVAL`] apart, the cadence Amazon ECS
//! reports a managed-scaling reservation at and the finest period an alarm over a
//! regular-resolution metric can be evaluated at. A series OPENS with [`RUN_UP_PERIODS`]
//! periods already behind it, published [`ACCELERATED_SPACING`] apart until its stamps reach the
//! present and one publish interval apart from then on: the real series has been running for as
//! long as the provider has, so the alarms the policy owns see as many closed periods of data as
//! their evaluation asks for instead of grading an empty window for minutes. Those two spans are
//! the emulator's own and they change the SCHEDULE alone — each datapoint reports the
//! reservation the records held when it was taken, whatever stamp the schedule gives it.
//!
//! The collection is owed by the CLOCK, whether or not anybody calls: it is driven from the same
//! pass that materialises the policy itself ([`settle_capacity_provider_reservation`], run by
//! the clock pass and by the clock pump behind it), which
//! [`ComAmazonawsEcsAmazonec2containerservicev20141113State::next_capacity_provider_reservation_due_at`]
//! keeps awake while a provider is being metered. Nothing it publishes depends on who called.
//!
//! The datapoints go into the CloudWatch emulator's OWN metric store — the same store
//! `PutMetricData` writes and `GetMetricStatistics` / `GetMetricData` / `ListMetrics` read —
//! reached through the shared engine, so one metric has one home and the publishers cannot
//! drift (the Container Insights counts and the `AWS/ECS` utilization series beside it take the
//! same path).
//!
//! LOCK ORDER: this service's store is read and its lock RELEASED before the Auto Scaling
//! store's is taken, and that one is released before CloudWatch's — the same one-way order
//! [`super::managed_scaling_policy`] keeps, which is what makes a lock cycle between the stores
//! unreachable.

use ::std::collections::{BTreeMap, HashMap, HashSet};

use ::service_com_amazonaws_autoscaling_autoscaling_2011_01_01::state as auto_scaling;
use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::container_insights::epoch_seconds;
use super::*;

/// What the series reads for a pair whose group holds no capacity and whose work asks for none:
/// the two are in BALANCE, so nothing is to be scaled either way, and 100 is the reading that
/// says so for a target-tracking policy — the value a provider's own documented default
/// `targetCapacity` holds the series at ([`MANAGED_SCALING_DEFAULT_TARGET_CAPACITY`]).
pub const BALANCED_RESERVATION: f64 = 100.0;

/// What it reads for a pair whose group holds no capacity at all while work waits for some:
/// twice the whole of a capacity that does not exist, which stands above every `targetCapacity`
/// a provider may be given (the member's published range ends at 100), so a group at zero
/// capacity is scaled out whatever target it carries.
pub const UNMET_CAPACITY_RESERVATION: f64 = 200.0;

/// The `StorageResolution` these datapoints are stored at, in seconds: the standard resolution
/// every metric a service publishes about itself takes ("standard resolution, with data having
/// a one-minute granularity", `PutMetricData`).
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// How many periods a series OPENS with, stamped one [`PUBLISH_INTERVAL`] apart ahead of the
/// instant the pair began being metered.
///
/// THE EMULATOR'S OWN FIGURE, not an AWS one, and the same one the `AWS/ECS` utilization series
/// opens with for the same reason ([`super::service_utilization`]): the real series has been
/// running for as long as the provider has, so an alarm created with it grades closed periods
/// that hold data instead of waiting minutes for its first.
const RUN_UP_PERIODS: u32 = 5;

/// How far apart the DATAPOINTS of one series come due while that series is still catching up
/// with the present.
///
/// THE EMULATOR'S OWN SPAN, and the publish schedule's alone: it is deliberately non-zero, so a
/// caller that reads the instant the wiring is created still finds the series opening rather
/// than complete.
const ACCELERATED_SPACING: Duration = Duration::from_millis(250);

/// What one `(capacity provider, cluster)` pair's reservation is READ as: the instances the
/// pair's work needs, over the instances the provider's group holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationReading {
    /// The instances the cluster's work through this provider needs: the group's instances it
    /// has placed work on, plus one for each task of it still waiting for capacity.
    pub needed: i32,
    /// The instances the provider's Auto Scaling group holds.
    pub held: i32,
}

impl ReservationReading {
    /// The percentage this reading publishes.
    #[must_use]
    pub fn percentage(self) -> f64 {
        let needed = self.needed.max(0);
        let held = self.held.max(0);
        if held == 0 {
            return if needed == 0 {
                BALANCED_RESERVATION
            } else {
                UNMET_CAPACITY_RESERVATION
            };
        }
        f64::from(needed) / f64::from(held) * 100.0
    }
}

/// One pair's place in its publish schedule: the stamp its next datapoint carries and when that
/// datapoint comes due.
#[derive(Debug)]
pub struct ReservationSeries {
    /// The time stamp the next datapoint carries. Advances by [`PUBLISH_INTERVAL`] per
    /// datapoint, so the series keeps the spacing the real one has.
    next_stamp: SystemTime,
    /// The instant the next datapoint comes due. Stored, never slept on.
    next_datapoint_at: SystemTime,
}

impl ReservationSeries {
    /// A series OPENING at `now`, with its run-up of periods already behind it.
    fn opening(now: SystemTime) -> Self {
        let run_up = PUBLISH_INTERVAL.saturating_mul(RUN_UP_PERIODS);
        Self {
            next_stamp: now.checked_sub(run_up).unwrap_or(now),
            // The first datapoint is due at once: the provider has been reserving capacity for
            // as long as it has existed.
            next_datapoint_at: now,
        }
    }

    /// Take the time stamp of the datapoint that is due, and schedule the next one.
    ///
    /// While the series is still behind the present the next datapoint is due one
    /// [`ACCELERATED_SPACING`] later; once its stamps have caught up, it is due when the clock
    /// reaches the stamp it carries. Never longer than one publish interval — the most the
    /// schedule can owe, so a clock that stepped backwards costs one slow datapoint rather than
    /// an unbounded wait — and never shorter than the accelerated spacing, so a catch-up cannot
    /// become a spin.
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

/// What ONE Auto Scaling group holds, as the reservation of every pair over it is read against:
/// how many instances the group has, and which of the account's Amazon EC2 instances they are.
#[derive(Debug, Clone, Default)]
struct GroupCapacity {
    /// The instances the group holds, as [`ReservationReading::held`] counts them.
    held: i32,
    /// Their Amazon EC2 instance ids, so the container instances of the cluster that are the
    /// GROUP's can be told from capacity its owner registered by hand.
    instance_ids: HashSet<String>,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// How many instances the work of ONE pair needs: the group's own container instances of
    /// that cluster which hold work, plus the tasks of the pair still waiting for capacity.
    ///
    /// An instance holds work when the cluster has a task on it that is still wanted running
    /// ([`Self::container_instance_tally`], the very counters `DescribeContainerInstances`
    /// reports), so what the series measures is what a client can read off the instances
    /// themselves.
    fn reservation_demand(&self, cluster_name: &str, group: &GroupCapacity, waiting: i32) -> i32 {
        let hosting = self
            .container_instances
            .values()
            .filter(|instance| instance.cluster_name == cluster_name)
            .filter(|instance| group.instance_ids.contains(&instance.ec2_instance_id))
            .filter(|instance| {
                let hosted = self.container_instance_tally(&instance.arn);
                hosted.running + hosted.pending > 0
            })
            .count();
        i32::try_from(hosting)
            .unwrap_or(i32::MAX)
            .saturating_add(waiting)
    }

    /// The reservation datapoints the pairs this store holds owe as of `now`, with each metered
    /// pair's series advanced to its next datapoint.
    ///
    /// A pair that is no longer metered has its series CLOSED, so a provider whose managed
    /// scaling was turned off, whose association was removed or which was deleted publishes
    /// nothing more and one that is wired again opens a new series with its own run-up.
    fn due_capacity_provider_reservations(
        &mut self,
        now: SystemTime,
        capacity: &HashMap<String, GroupCapacity>,
    ) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
        // Exactly the pairs that HAVE a policy, read through the one reader that decides it,
        // narrowed to the ones whose group Amazon EC2 Auto Scaling really holds: a group that
        // service holds none of has no capacity to report, and nothing here invents a reading
        // for it.
        let metered: Vec<ManagedScalingPolicy> = self
            .managed_scaling_policies()
            .into_iter()
            .filter(|policy| capacity.contains_key(&policy.group_name))
            .collect();
        let keys: HashSet<(String, String)> = metered
            .iter()
            .map(|policy| (policy.cluster_name.clone(), policy.provider_name.clone()))
            .collect();
        self.capacity_provider_reservation
            .retain(|key, _| keys.contains(key));
        for key in keys {
            self.capacity_provider_reservation
                .entry(key)
                .or_insert_with(|| ReservationSeries::opening(now));
        }
        // What each pair that owes a datapoint READS, taken before any series is advanced: the
        // reading is taken off the whole store, so it cannot be taken while one series is held.
        let waiting = self.tasks_awaiting_managed_capacity();
        let readings: Vec<(ManagedScalingPolicy, ReservationReading)> = metered
            .into_iter()
            .filter(|policy| {
                self.capacity_provider_reservation
                    .get(&(policy.cluster_name.clone(), policy.provider_name.clone()))
                    .is_some_and(|series| series.next_datapoint_at <= now)
            })
            .map(|policy| {
                let group = capacity
                    .get(&policy.group_name)
                    .cloned()
                    .unwrap_or_default();
                let parked = waiting
                    .get(&(policy.cluster_name.clone(), policy.provider_name.clone()))
                    .copied()
                    .unwrap_or_default();
                let reading = ReservationReading {
                    needed: self.reservation_demand(&policy.cluster_name, &group, parked),
                    held: group.held,
                };
                (policy, reading)
            })
            .collect();
        let mut published = Vec::with_capacity(readings.len());
        for (policy, reading) in readings {
            let key = (policy.cluster_name.clone(), policy.provider_name.clone());
            let Some(series) = self.capacity_provider_reservation.get_mut(&key) else {
                continue;
            };
            let stamp = series.advance(now);
            let sample = reading.percentage();
            let dimensions: BTreeMap<String, String> = [
                (
                    CAPACITY_PROVIDER_NAME_DIMENSION.to_string(),
                    policy.provider_name.clone(),
                ),
                (
                    CLUSTER_NAME_DIMENSION.to_string(),
                    policy.cluster_name.clone(),
                ),
            ]
            .into_iter()
            .collect();
            published.push((
                cloudwatch::MetricKey {
                    namespace: MANAGED_SCALING_METRIC_NAMESPACE.to_string(),
                    metric_name: MANAGED_SCALING_METRIC_NAME.to_string(),
                    dimensions,
                },
                cloudwatch::StoredDatum {
                    timestamp_secs: epoch_seconds(stamp),
                    // One observation: this collection's own reading of the pair.
                    sample_count: 1.0,
                    sum: sample,
                    minimum: sample,
                    maximum: sample,
                    unit: Some(PERCENT_UNIT.to_string()),
                    storage_resolution: STORAGE_RESOLUTION_SECS,
                },
            ));
        }
        published
    }

    /// When the next datapoint any reservation series this store is publishing comes due, if
    /// any.
    ///
    /// What keeps the clock pump awake while a provider is being metered: the series is
    /// published for as long as the provider's managed scaling stands, and no call is made
    /// between one datapoint and the next ([`super::task_events::drive_task_clock`]).
    pub(super) fn next_capacity_provider_reservation_due_at(&self) -> Option<SystemTime> {
        self.capacity_provider_reservation
            .values()
            .map(|series| series.next_datapoint_at)
            .min()
    }
}

/// What each group the metered pairs are over HOLDS, read under the Auto Scaling store's lock
/// alone and answered as owned values (see the module's LOCK ORDER note).
///
/// A group that service does not hold is left out, so a pair over it publishes nothing: the
/// capacity behind a provider is that service's own fact, and this reports no reading for a
/// group a read of it would not resolve. An engine carrying no Auto Scaling store holds no group
/// at all, so nothing is metered.
fn held_group_capacity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    groups: &HashSet<String>,
) -> HashMap<String, GroupCapacity> {
    let Some(auto_scaling_state) = auto_scaling::service_state(engine) else {
        return HashMap::new();
    };
    let store = auto_scaling_state.lock();
    groups
        .iter()
        .filter_map(|group_name| {
            let group = store.groups.get(group_name)?;
            // Every member the group still holds, on its way into service or in it: a group
            // whose instance is coming up is already carrying that capacity, which is what
            // keeps the reservation from asking for it twice. An instance that LEFT the group
            // without being terminated is none of its own, exactly as that service's own
            // counts read it.
            let instance_ids: HashSet<String> = group
                .instances
                .iter()
                .filter(|instance| {
                    instance.lifecycle_state != auto_scaling::LIFECYCLE_DETACHING
                        && instance.lifecycle_state != auto_scaling::LIFECYCLE_DETACHED
                })
                .map(|instance| instance.instance_id.clone())
                .collect();
            Some((
                group_name.clone(),
                GroupCapacity {
                    held: i32::try_from(instance_ids.len()).unwrap_or(i32::MAX),
                    instance_ids,
                },
            ))
        })
        .collect()
}

/// Publish the `CapacityProviderReservation` datapoints the clock owes the capacity providers
/// this store holds — the ONE call the clock pass makes for it.
///
/// Called with NO guard of this store held: it reads this store, releases its lock, reads the
/// Auto Scaling store, releases that, takes this store's lock again to advance the series it is
/// publishing, and only then writes the CloudWatch store — the one-directional, guard-free
/// discipline every other cross-store access in this crate follows, which is what keeps a cycle
/// between the stores unreachable.
///
/// Nothing is published on an engine that holds no metric store to reach: a datapoint has one
/// home, and this path never makes a second one.
pub fn settle_capacity_provider_reservation<Eng: Send + Sync + 'static>(
    engine: &Eng,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The groups the metered pairs are over, read through the one reader that decides which
    // pairs are metered at all. The guard this statement takes is a TEMPORARY: it is released
    // at the end of the statement, so nothing below holds it.
    let groups: HashSet<String> = state
        .lock()
        .managed_scaling_policies()
        .into_iter()
        .map(|policy| policy.group_name)
        .collect();
    let capacity = if groups.is_empty() {
        HashMap::new()
    } else {
        held_group_capacity(engine, &groups)
    };
    let published = state
        .lock()
        .due_capacity_provider_reservations(now, &capacity);
    if published.is_empty() {
        return;
    }
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
                "the CloudWatch metric store is at capacity, so this capacity-provider \
                 reservation datapoint was dropped"
            );
            continue;
        }
        written.push(cloudwatch::WrittenSeries {
            key: watched,
            units: vec![unit],
        });
    }
    // The alarms the managed target-tracking policy owns are evaluated FROM these datapoints,
    // so storing the series they watch is what makes their evaluation due — an alarm cannot
    // tell this series from one a client published.
    store.make_alarms_due(&written, epoch_seconds(now));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: what the pair's work needs, what its group holds, and the
    /// percentage the series then reads.
    #[derive(Debug, Clone, Copy)]
    struct Case {
        case: &'static str,
        needed: i32,
        held: i32,
        expected: f64,
    }

    /// THE RATIO: the instances the work needs over the instances the group holds, times a
    /// hundred — with the two readings a ratio cannot express stated on their own.
    #[test]
    fn the_reservation_is_the_needed_capacity_over_the_held_capacity() {
        let rows: &[Case] = &[
            Case {
                case: "nothing held and nothing needed: in balance",
                needed: 0,
                held: 0,
                expected: BALANCED_RESERVATION,
            },
            Case {
                case: "nothing held while work waits: scale out from zero",
                needed: 1,
                held: 0,
                expected: UNMET_CAPACITY_RESERVATION,
            },
            Case {
                case: "every instance the group holds is working: the whole of it",
                needed: 2,
                held: 2,
                expected: 100.0,
            },
            Case {
                case: "half the group is working",
                needed: 1,
                held: 2,
                expected: 50.0,
            },
            Case {
                case: "the group holds capacity nothing is working: none of it",
                needed: 0,
                held: 4,
                expected: 0.0,
            },
            Case {
                case: "more instances are needed than the group holds: over the whole of it",
                needed: 3,
                held: 2,
                expected: 150.0,
            },
        ];
        for row in rows {
            let read = ReservationReading {
                needed: row.needed,
                held: row.held,
            }
            .percentage();
            let case = row.case;
            let expected = row.expected;
            assert!(
                (read - expected).abs() < f64::EPSILON,
                "case {case:?}: expected the series to read {expected}, got {read}"
            );
        }
    }
}
