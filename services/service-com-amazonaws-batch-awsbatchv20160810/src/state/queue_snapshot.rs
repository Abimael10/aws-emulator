// SPDX-License-Identifier: CC0-1.0

//! What `GetJobQueueSnapshot` reports about ONE queue: the entries at the front of it, and
//! the capacity its dispatched jobs are using.
//!
//! The operation's own doc: "a snapshot of job queue state, including ordering of RUNNABLE
//! jobs, as well as capacity utilization for already dispatched jobs. The first 100 RUNNABLE
//! jobs in the job queue are listed in order of dispatch. For job queues with an attached
//! quota-share policy, the first RUNNABLE job in each quota share is also listed. Capacity
//! utilization for the job queue is provided, as well as break downs by share for job queues
//! with attached fair-share or quota-share scheduling policies."
//!
//! A queue holds whichever kind of job its type takes — a `SAGEMAKER_TRAINING` queue holds
//! SERVICE jobs ([`ServiceJob`]), every other kind holds container jobs ([`Job`]) — so
//! "the RUNNABLE jobs of this queue" is both of them, and the snapshot reports a queue's own
//! jobs whichever kind they are.
//!
//! Capacity utilization is "for already DISPATCHED jobs", so what it counts is what a job is
//! HOLDING right now ([`ServiceJob::holds_capacity`]): a job still waiting in the queue has
//! been given nothing, and one that has finished has given back whatever it held. The
//! quantities are the ones derived when each submit was accepted
//! ([`requested_capacity`]), so a snapshot re-reads no payload.

use super::*;

/// One entry at the front of a queue: the job's ARN and the instant it has been at its
/// position since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedEntry {
    /// The ARN of the job the entry is for.
    pub job_arn: String,
    /// `earliestTimeAtPosition`, in epoch milliseconds.
    pub earliest_time_at_position: Option<i64>,
    /// The share the job named, if any: `shareIdentifier` for a fair-share queue.
    pub share_identifier: Option<String>,
    /// The quota share the job named, if any.
    pub quota_share_name: Option<String>,
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The entries at the front of the queue `arn`, in the order that queue dispatches them,
    /// capped at [`MAX_FRONT_OF_QUEUE_JOBS`] ("the first 100 RUNNABLE jobs").
    ///
    /// A queue holds container jobs or service jobs, never both — `serviceEnvironmentOrder`'s
    /// own doc: "a job queue can't have both a `serviceEnvironmentOrder` and a
    /// `computeEnvironmentOrder` field" — so this is the one of the two that has jobs waiting
    /// on it.
    pub fn front_of_queue(&self, arn: &str) -> Vec<QueuedEntry> {
        let mut entries: Vec<QueuedEntry> = self
            .runnable_jobs_of_queue(arn)
            .into_iter()
            .map(|job| QueuedEntry {
                job_arn: job.job_arn.clone(),
                earliest_time_at_position: job.at_position_since,
                share_identifier: job.share_identifier.clone(),
                quota_share_name: None,
            })
            .collect();
        entries.extend(
            self.runnable_service_jobs_of_queue(arn)
                .into_iter()
                .map(|job| QueuedEntry {
                    job_arn: job.job_arn.clone(),
                    // A service job has been at its position since it was submitted: nothing
                    // reorders a service job once it is waiting, so the instant it entered
                    // the queue is the instant it reached the position it is at.
                    earliest_time_at_position: Some(job.created_at),
                    share_identifier: job.share_identifier.clone(),
                    quota_share_name: job.quota_share_name.clone(),
                }),
        );
        entries.truncate(MAX_FRONT_OF_QUEUE_JOBS);
        entries
    }

    /// The `RUNNABLE` service jobs of the queue `arn`, in the order that queue dispatches
    /// them.
    ///
    /// The same ordering the queue's container jobs are in
    /// ([`Self::runnable_jobs_of_queue`]): the job's own `schedulingPriority` decides first on
    /// a queue that dispatches by priority ([`Self::queue_orders_by_priority`]), and
    /// submission order — the `createdAt` the submit recorded, with the job id breaking a tie
    /// — decides the rest, so one instant answers one order.
    pub fn runnable_service_jobs_of_queue(&self, arn: &str) -> Vec<&ServiceJob> {
        let by_priority = self.queue_orders_by_priority(arn);
        let mut queued: Vec<&ServiceJob> = self
            .service_jobs
            .values()
            .filter(|job| job.job_queue_arn == arn && job.status == SERVICE_JOB_RUNNABLE)
            .collect();
        queued.sort_by(|left, right| {
            let key = |job: &ServiceJob| {
                if by_priority {
                    job.scheduling_priority
                        .unwrap_or(MIN_SCHEDULING_PRIORITY)
                        .saturating_neg()
                } else {
                    0
                }
            };
            key(left)
                .cmp(&key(right))
                .then(left.created_at.cmp(&right.created_at))
                .then(left.job_id.cmp(&right.job_id))
        });
        queued
    }

    /// The service jobs of the queue `arn` that are HOLDING capacity right now.
    pub fn dispatched_service_jobs_of_queue(&self, arn: &str) -> Vec<&ServiceJob> {
        let mut dispatched: Vec<&ServiceJob> = self
            .service_jobs
            .values()
            .filter(|job| job.job_queue_arn == arn && job.holds_capacity())
            .collect();
        dispatched.sort_by(|left, right| left.job_id.cmp(&right.job_id));
        dispatched
    }

    /// How much capacity the queue `arn`'s dispatched jobs are using, per unit, summed over
    /// them — the queue's `totalCapacityUsage`.
    pub fn queue_capacity_usage(&self, arn: &str) -> Vec<(String, f64)> {
        let mut per_unit: BTreeMap<String, f64> = BTreeMap::new();
        for job in self.dispatched_service_jobs_of_queue(arn) {
            for (unit, quantity) in job.capacity_usage().unwrap_or_default() {
                *per_unit.entry(unit).or_insert(0.0) += quantity;
            }
        }
        per_unit.into_iter().collect()
    }

    /// The same, broken down by the share each dispatched job named: one entry per share,
    /// each with its own per-unit usage.
    ///
    /// `share` picks which of the two share members names the share — `shareIdentifier` on a
    /// fair-share queue, `quotaShareName` on a quota-share one — so the one breakdown serves
    /// both of the reports the operation documents. A job that names no share of that kind is
    /// in no share, so it is counted in the queue's total and in no breakdown.
    pub fn queue_capacity_usage_by_share(
        &self,
        arn: &str,
        share: ShareDimension,
    ) -> Vec<(String, Vec<(String, f64)>)> {
        let mut per_share: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
        for job in self.dispatched_service_jobs_of_queue(arn) {
            let Some(name) = (match share {
                ShareDimension::FairShare => job.share_identifier.clone(),
                ShareDimension::QuotaShare => job.quota_share_name.clone(),
            }) else {
                continue;
            };
            let holding = per_share.entry(name).or_default();
            for (unit, quantity) in job.capacity_usage().unwrap_or_default() {
                *holding.entry(unit).or_insert(0.0) += quantity;
            }
        }
        per_share
            .into_iter()
            .map(|(name, holding)| (name, holding.into_iter().collect()))
            .collect()
    }

    /// How many shares of `arn` have a dispatched job in them — `activeShareCount`.
    pub fn active_share_count(&self, arn: &str, share: ShareDimension) -> i64 {
        i64::try_from(self.queue_capacity_usage_by_share(arn, share).len()).unwrap_or_default()
    }

    /// How much capacity, per unit, the service jobs already dispatched against the service
    /// environment `environment_arn` are holding.
    ///
    /// A service job is placed into one of the environments its queue orders, and which one
    /// is not recorded, so a job of a queue that orders this environment counts against it.
    /// A queue ordering one environment — the shape `serviceEnvironmentOrder` takes for every
    /// queue this emulator creates — makes that exact.
    pub fn environment_capacity_in_use(&self, environment_arn: &str) -> BTreeMap<String, i32> {
        let mut per_unit: BTreeMap<String, i32> = BTreeMap::new();
        for job in self
            .service_jobs
            .values()
            .filter(|job| job.is_allocated_capacity())
        {
            let orders = self
                .job_queue(&job.job_queue_arn)
                .and_then(|queue| queue.service_environment_order.as_ref())
                .is_some_and(|order| {
                    order.iter().any(|entry| {
                        self.service_environment(&entry.service_environment)
                            .is_some_and(|environment| environment.arn == environment_arn)
                    })
                });
            if orders {
                add_demand(&mut per_unit, job);
            }
        }
        per_unit
    }

    /// How much capacity, per unit, the service jobs already dispatched in the quota share
    /// `name` of the queue `queue_arn` are holding.
    pub fn quota_share_capacity_in_use(
        &self,
        queue_arn: &str,
        name: &str,
    ) -> BTreeMap<String, i32> {
        let mut per_unit: BTreeMap<String, i32> = BTreeMap::new();
        for job in self.service_jobs.values().filter(|job| {
            job.is_allocated_capacity()
                && job.job_queue_arn == queue_arn
                && job.quota_share_name.as_deref() == Some(name)
        }) {
            add_demand(&mut per_unit, job);
        }
        per_unit
    }

    /// Whether the quota share `job` named still has room for what its request asks for.
    ///
    /// `QuotaShareCapacityLimit`: "the maximum quantity of a resource that can be allocated
    /// to jobs in the quota share WITHOUT BORROWING", so a share's own limits bound the jobs
    /// in it — unless its sharing strategy lets it borrow idle capacity from another share
    /// ([`QUOTA_SHARE_STRATEGY_LEND_AND_BORROW`], whose default `borrowLimit` of -1
    /// "indicates unlimited borrowing"), in which case the share publishes no bound of its own
    /// and only the environment's limits hold the job back. A job that names no share, or one
    /// whose share this store no longer holds, is in no share, so nothing bounds it here.
    pub fn quota_share_admits(&self, job: &ServiceJob) -> bool {
        let Some(name) = job.quota_share_name.as_deref() else {
            return true;
        };
        let Some(share) = self.quota_share_on_queue(&job.job_queue_arn, name) else {
            return true;
        };
        if share.resource_sharing_configuration.strategy == QUOTA_SHARE_STRATEGY_LEND_AND_BORROW {
            return true;
        }
        let limits: Vec<CapacityLimit> = share
            .capacity_limits
            .iter()
            .map(|limit| CapacityLimit {
                max_capacity: Some(limit.max_capacity),
                capacity_unit: Some(limit.capacity_unit.clone()),
            })
            .collect();
        capacity_admits_with_usage(
            &limits,
            &job.requested_capacity,
            &self.quota_share_capacity_in_use(&job.job_queue_arn, name),
        )
    }
}

/// The `QuotaShareResourceSharingStrategy` member that lets a share BORROW idle capacity
/// from another share (`smithy_query(query="shape",
/// shape="QuotaShareResourceSharingStrategy")`: RESERVE | LEND | LEND_AND_BORROW).
pub const QUOTA_SHARE_STRATEGY_LEND_AND_BORROW: &str = "LEND_AND_BORROW";

/// Add what `job` is holding to `per_unit`.
///
/// The unit is the one the job's own demand names — the instance type it asked for, else
/// [`CAPACITY_UNIT_NUM_INSTANCES`] — so it is the same key the capacity reports count under
/// ([`ServiceJob::capacity_usage`]).
fn add_demand(per_unit: &mut BTreeMap<String, i32>, job: &ServiceJob) {
    for demand in &job.requested_capacity {
        let unit = demand
            .instance_type
            .clone()
            .unwrap_or_else(|| CAPACITY_UNIT_NUM_INSTANCES.to_string());
        let held = per_unit.entry(unit).or_insert(0);
        *held = held.saturating_add(demand.instances);
    }
}

/// Which of a job's two share members names the share a report breaks capacity down by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareDimension {
    /// `shareIdentifier`, which is the share a FAIR-SHARE policy weighs.
    FairShare,
    /// `quotaShareName`, which is the share a QUOTA-SHARE policy allocates.
    QuotaShare,
}
