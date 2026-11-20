// SPDX-License-Identifier: CC0-1.0

//! WHICH of the `RUNNABLE` jobs waiting on a compute environment the scheduler admits onto
//! its capacity, and in WHAT ORDER.
//!
//! # The rule this module implements
//!
//! `ComputeResource$maxvCpus` is "the maximum number of vCPUs that a compute environment can
//! support", and `ResourceRequirement$value` for `VCPU` is "the number of vCPUs reserved for
//! the container" — so the maximum is a CONCURRENCY bound on the jobs an environment holds:
//! the vCPUs reserved by the jobs placed on one environment never exceed its maximum, and a
//! `RUNNABLE` job whose reservation does not fit what is left WAITS where a job waits to be
//! placed ("the job is ready to be scheduled to a host", `JobStatus$RUNNABLE`) until a job
//! holding capacity reaches a terminal status and gives it back.
//!
//! That bound is what makes the SCHEDULING ORDER observable, and two documented rules decide
//! it:
//!
//! * across the queues that share one environment, `CreateJobQueue$priority` — "job queues
//!   with a higher priority (or a higher integer value for the priority parameter) are
//!   evaluated first when associated with the same compute environment. Priority is
//!   determined in descending order" — so the queues of one environment are evaluated from
//!   the highest `priority` down;
//! * within one queue, the queue's own scheduling policy. On a FAIR-SHARE queue
//!   `GetJobQueueSnapshot` reports its `RUNNABLE` jobs "ordered based on their job priority
//!   and share usage", and `ShareAttributes$weightFactor` is "the weight factor for the share
//!   identifier. The default value is 1.0. A lower value has a higher priority for compute
//!   resources" — so a share's USAGE, weighted by its own factor, is what ranks the jobs of
//!   one queue, and `JobDetail$schedulingPriority` ("jobs with a higher scheduling priority
//!   are scheduled before jobs with a lower scheduling priority") decides between the jobs
//!   that rank equally. A queue with no policy attached is first-in-first-out, "ordered based
//!   on their submission time".
//!
//! Nothing here is invented. Every quantity is read off records a request wrote: the
//! environment's own maximum, the vCPUs each job's container reserves ([`job_vcpus`]), the
//! `shareIdentifier` each job named and the weight its queue's policy gives that share. The
//! usage a share carries is what the jobs of that share have been GIVEN — what they hold now,
//! plus what the ones that have finished held, over the window the policy measures usage over
//! (`FairsharePolicy$shareDecaySeconds`) — so the ordering a reader sees follows from the
//! placements the service made and from nothing else.
//!
//! # One order, two readers
//!
//! The order computed here is the order `GetJobQueueSnapshot` reports a queue's front in
//! ([`ComAmazonawsBatchAwsbatchv20160810State::runnable_jobs_of_queue`]) AND the order the
//! scheduler admits jobs in ([`ComAmazonawsBatchAwsbatchv20160810State::capacity_admits`]),
//! so what a client is shown at the front of a queue is what runs next.
//!
//! Admission is decided for each job against the jobs AHEAD of it: the candidates eligible at
//! this instant are walked in order and each is given the capacity it reserves until what is
//! left cannot hold the next one, so two jobs that each fit the free capacity alone are never
//! both admitted when together they exceed it. The decision is a pure function of the stored
//! records, so it does not depend on the order in which the clock happens to look at the jobs
//! that came due.

use ::std::cmp::Ordering;

use super::*;

/// What ONE candidate for a compute environment's capacity is ordered by, read off the job
/// once for the whole sort.
///
/// Lowest first, member by member: the queue's `priority` (negated, so the higher priority is
/// evaluated first), what the job's share has cost it in its queue's ordering, the job's own
/// `schedulingPriority` (negated by the rank that answers it), the instant its submit was
/// recorded, its place in its queue's submission order — so two jobs submitted inside one
/// millisecond are still evaluated in the order they were submitted — and finally its id, so
/// one instant answers one order.
///
/// The share cost is compared only AFTER the queue's priority, so the two costs being weighed
/// against each other are normally one queue's — which is the queue whose own policy produced
/// them. Two queues of EQUAL priority sharing an environment are evaluated together, and there
/// the cost still ranks the job whose share has been given the least of that environment
/// first, which is what the shares of either queue are weighed for; a queue with no policy
/// carries no cost at all, so its jobs are ordered by the members below it.
#[derive(Debug, Clone, Copy)]
struct DispatchKey<'job> {
    queue_priority: i32,
    share_cost: f64,
    scheduling_priority: i32,
    created_at: i64,
    queue_position: usize,
    job_id: &'job str,
}

impl DispatchKey<'_> {
    /// This key against `other`, member by member in the order above.
    fn compare(&self, other: &Self) -> Ordering {
        self.queue_priority
            .cmp(&other.queue_priority)
            .then(self.share_cost.total_cmp(&other.share_cost))
            .then(self.scheduling_priority.cmp(&other.scheduling_priority))
            .then(self.created_at.cmp(&other.created_at))
            .then(self.queue_position.cmp(&other.queue_position))
            .then(self.job_id.cmp(other.job_id))
    }
}

impl ComputeEnvironment {
    /// The vCPUs this environment can hold at one instant (`ComputeResource$maxvCpus`), or
    /// `None` when it publishes no maximum.
    ///
    /// `minvCpus` is "the minimum number of vCPUs that a compute environment should maintain",
    /// which capacity is already running, so the ceiling is never below it — the same reading
    /// the scale-out takes ([`ComputeEnvironment::scale_to`]). An environment with no
    /// `computeResources` at all manages no capacity of its own (`computeResources` is
    /// "required for managed compute environments"), and a stored maximum of zero is a
    /// request that stated none, so neither publishes a bound and neither holds a job back.
    pub fn vcpu_ceiling(&self) -> Option<i32> {
        let resources = self.compute_resources.as_ref()?;
        let ceiling = resources
            .maxv_cpus
            .max(resources.minv_cpus.unwrap_or_default());
        (ceiling > 0).then_some(ceiling)
    }
}

impl Job {
    /// Whether this job is HOLDING compute capacity right now.
    ///
    /// A job holds the vCPUs its container reserves from the instant the scheduler admitted it
    /// — its container is being initiated (`STARTING`), running (`RUNNING`), or owed to the
    /// Amazon ECS task it was admitted as ([`JobTaskDispatch`], which leaves the job reported
    /// in `RUNNABLE` until that task exists) — until it reaches a terminal status, after which
    /// it can run nothing more. A job still waiting in its queue has been given nothing, and
    /// an array PARENT runs no container of its own, so it reserves nothing: its children do.
    pub fn holds_compute_capacity(&self) -> bool {
        if is_terminal_status(&self.status) || self.array_size.is_some() {
            return false;
        }
        self.status == JOB_STARTING
            || self.status == JOB_RUNNING
            || self.task_dispatch != JobTaskDispatch::Waiting
    }
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The vCPUs the jobs placed on the compute environment `environment_arn` are holding,
    /// summed.
    ///
    /// A placed job holds capacity on the environment the scheduler would place it on — the
    /// same environment its queue's `computeEnvironmentOrder` picks for it
    /// ([`Self::placement_environment`]), which is the one it was placed on — so a job of
    /// another environment's queues is not counted against this one.
    ///
    /// Costs one walk of the jobs map, which [`limits::MAX_JOBS`] bounds.
    pub fn vcpus_held_on(&self, environment_arn: &str) -> i32 {
        self.jobs
            .values()
            .filter(|job| job.holds_compute_capacity())
            .filter(|job| {
                self.placement_environment(&job.job_id)
                    .is_some_and(|environment| environment.arn == environment_arn)
            })
            .fold(0_i32, |held, job| held.saturating_add(job_vcpus(job)))
    }

    /// The fair-share policy attached to the queue `arn`, or `None` for a queue with no
    /// policy — or one whose policy is a quota-share policy, which allocates named shares
    /// instead ([`SchedulingPolicy`] carries one or the other and never both).
    fn queue_fairshare_policy(&self, arn: &str) -> Option<&FairsharePolicy> {
        self.job_queue(arn)
            .and_then(|queue| queue.scheduling_policy_arn.as_deref())
            .and_then(|policy_arn| self.scheduling_policy(policy_arn))
            .and_then(|policy| policy.fairshare_policy.as_ref())
    }

    /// The `weightFactor` the queue `arn`'s fair-share policy gives `share`.
    ///
    /// "Share identifiers that aren't included have a default weight of `1.0`"
    /// ([`DEFAULT_WEIGHT_FACTOR`]), and an entry whose identifier ends with an asterisk
    /// "specifies the weight factor to use for share identifiers that start with that prefix"
    /// — so an exact entry is a prefix of zero asterisks and the LONGEST matching prefix
    /// decides, which for a policy whose identifiers may not overlap
    /// ([`overlapping_share_identifiers`]) is simply the one entry that matches.
    ///
    /// The factor is held at or above the smallest value the member publishes
    /// ([`MIN_WEIGHT_FACTOR`]), so a share can never be weighted to nothing.
    fn share_weight_factor(&self, arn: &str, share: Option<&str>) -> f64 {
        let Some(policy) = self.queue_fairshare_policy(arn) else {
            return f64::from(DEFAULT_WEIGHT_FACTOR);
        };
        let share = share.unwrap_or_default();
        let weight = policy
            .share_distribution
            .iter()
            .flatten()
            .filter_map(|attributes| {
                let declared = attributes.share_identifier.as_str();
                let prefix = declared.strip_suffix('*');
                let matched = match prefix {
                    Some(prefix) => share.starts_with(prefix),
                    None => declared == share,
                };
                matched.then(|| {
                    (
                        prefix.unwrap_or(declared).chars().count(),
                        attributes.weight_factor.unwrap_or(DEFAULT_WEIGHT_FACTOR),
                    )
                })
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, weight)| weight)
            .unwrap_or(DEFAULT_WEIGHT_FACTOR);
        f64::from(weight.max(MIN_WEIGHT_FACTOR))
    }

    /// The window over which the queue `arn` measures a share's usage, in milliseconds.
    ///
    /// `FairsharePolicy$shareDecaySeconds` is "the amount of time (in seconds) to use to
    /// calculate a fair-share percentage for each share identifier in use. A value of zero
    /// (0) indicates the default minimum time window (600 seconds)" — so the policy's own
    /// value decides, and a policy that states none, or states the documented zero, measures
    /// over that default window ([`DEFAULT_SHARE_DECAY_SECONDS`]).
    fn share_usage_window_millis(&self, arn: &str) -> i64 {
        let seconds = self
            .queue_fairshare_policy(arn)
            .and_then(|policy| policy.share_decay_seconds)
            .filter(|seconds| *seconds > MIN_SHARE_DECAY_SECONDS)
            .unwrap_or(DEFAULT_SHARE_DECAY_SECONDS);
        i64::from(seconds).saturating_mul(1_000)
    }

    /// The vCPUs `share` has USED of the queue `arn` as at `now`: what its jobs are holding
    /// right now, plus what the ones that have already finished held, over the window the
    /// queue's policy measures usage over ([`Self::share_usage_window_millis`]).
    ///
    /// A share's usage is what the scheduler has already given it, so it does not vanish the
    /// instant a job of that share ends: a share that has just had the queue's capacity is
    /// still the busy one while the window lasts, which is what gives a share that has had
    /// none its turn when capacity frees. A job that was never placed used nothing, whatever
    /// ended it, and one that finished longer ago than the window is no longer counted.
    ///
    /// A job that named no `shareIdentifier` is in no share, so it is counted in the usage of
    /// the unnamed group and in none of the named ones.
    fn share_vcpus_used(&self, arn: &str, share: Option<&str>, now: i64) -> i32 {
        let window = self.share_usage_window_millis(arn);
        self.jobs_by_queue
            .get(arn)
            .into_iter()
            .flatten()
            .filter_map(|id| self.jobs.get(id))
            .filter(|job| job.share_identifier.as_deref() == share)
            .filter(|job| {
                if job.holds_compute_capacity() {
                    return true;
                }
                let was_placed = job.started_at.is_some() || !job.attempts.is_empty();
                was_placed
                    && job
                        .ended_at
                        .is_some_and(|ended| now.saturating_sub(ended) < window)
            })
            .fold(0_i32, |used, job| used.saturating_add(job_vcpus(job)))
    }

    /// What `job`'s SHARE costs it in its queue's ordering, given the vCPUs `used` that share
    /// has had ([`Self::share_vcpus_used`]): the usage weighted by the share's own
    /// `weightFactor`, and zero on a queue that weighs no shares at all.
    ///
    /// Lower comes first, so a share that has used nothing is ahead of one that has had
    /// capacity, and between two shares that have used the same a LOWER weight factor is
    /// ahead — which is what "a lower value has a higher priority for compute resources"
    /// says, and what makes a share weighted at an eighth of another get eight times the
    /// capacity before it falls behind it.
    ///
    /// The usage is passed in rather than read here, so a sort that ranks many jobs of one
    /// share reads that share's usage once ([`Self::dispatch_candidates`]).
    fn share_cost_of_usage(&self, job: &Job, used: i32) -> f64 {
        let arn = job.job_queue_arn.as_str();
        if self.queue_fairshare_policy(arn).is_none() {
            return 0.0;
        }
        let share = job.share_identifier.as_deref();
        f64::from(used) * self.share_weight_factor(arn, share)
    }

    /// The vCPUs the share `job` named has used of its queue as at `now`, read through
    /// `cache` so a sort over many jobs of one share reads that share's usage once.
    fn share_usage_cached<'job>(
        &self,
        job: &'job Job,
        now: i64,
        cache: &mut HashMap<(&'job str, Option<&'job str>), i32>,
    ) -> i32 {
        let share = (
            job.job_queue_arn.as_str(),
            job.share_identifier.as_deref(),
        );
        match cache.get(&share) {
            Some(used) => *used,
            None => {
                let used = self.share_vcpus_used(share.0, share.1, now);
                cache.insert(share, used);
                used
            }
        }
    }

    /// `job`'s own `schedulingPriority` as its queue's ordering reads it: NEGATED, so the
    /// higher priority is dispatched first.
    ///
    /// Read only for a queue that dispatches by it ([`Self::queue_orders_by_priority`]); every
    /// other queue is first-in-first-out. A job that names no `schedulingPriority` is ordered
    /// at the lowest the member documents ("The minimum supported value is 0",
    /// [`MIN_SCHEDULING_PRIORITY`]).
    fn scheduling_priority_key(&self, job: &Job) -> i32 {
        if self.queue_orders_by_priority(&job.job_queue_arn) {
            job.scheduling_priority
                .unwrap_or(MIN_SCHEDULING_PRIORITY)
                .saturating_neg()
        } else {
            MIN_SCHEDULING_PRIORITY
        }
    }

    /// `queued` — the `RUNNABLE` jobs of ONE queue, in submission order — in the order that
    /// queue dispatches them as at `now`: what each job's share has cost it
    /// ([`Self::share_cost_of_usage`]) and then its own `schedulingPriority`, lowest first.
    ///
    /// Each key is read once and each share's usage once, however many of its jobs are in the
    /// list. Submission order is what decides between jobs with EQUAL keys, and that order
    /// needs no key: [`Self::jobs_by_queue`] holds a queue's jobs in submission order and the
    /// sort is STABLE, so equal keys come out as submitted, whatever resolution the clock
    /// reports. `GetJobQueueSnapshot`'s `frontOfQueue` doc: "For first-in-first-out (FIFO) job
    /// queues, jobs are ordered based on their submission time."
    pub(super) fn in_queue_dispatch_order<'job>(
        &self,
        queued: Vec<&'job Job>,
        now: i64,
    ) -> Vec<&'job Job> {
        let mut used_by_share: HashMap<(&'job str, Option<&'job str>), i32> = HashMap::new();
        let mut ranked: Vec<((f64, i32), &'job Job)> = Vec::with_capacity(queued.len());
        for job in queued {
            let used = self.share_usage_cached(job, now, &mut used_by_share);
            ranked.push((
                (
                    self.share_cost_of_usage(job, used),
                    self.scheduling_priority_key(job),
                ),
                job,
            ));
        }
        ranked.sort_by(|(left, _), (right, _)| {
            left.0.total_cmp(&right.0).then(left.1.cmp(&right.1))
        });
        ranked.into_iter().map(|(_, job)| job).collect()
    }

    /// The `priority` of the queue `job` was submitted to, as the cross-queue evaluation
    /// reads it: "job queues with a higher priority ... are evaluated first when associated
    /// with the same compute environment."
    fn queue_priority_of(&self, job: &Job) -> i32 {
        self.job_queue(&job.job_queue_arn)
            .map(|queue| queue.priority)
            .unwrap_or_default()
    }

    /// The key one candidate for a compute environment's capacity is ordered by, read off the
    /// job once for the whole sort ([`DispatchKey`]).
    ///
    /// `used` is the vCPUs the job's share has had, which the caller reads once per share.
    fn dispatch_key<'job>(
        &self,
        job: &'job Job,
        used: i32,
        position_of: &HashMap<&str, usize>,
    ) -> DispatchKey<'job> {
        DispatchKey {
            // Negated, so the HIGHER queue priority sorts first: "priority is determined in
            // descending order".
            queue_priority: self.queue_priority_of(job).saturating_neg(),
            share_cost: self.share_cost_of_usage(job, used),
            scheduling_priority: self.scheduling_priority_key(job),
            created_at: job.created_at,
            queue_position: position_of
                .get(job.job_id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            job_id: job.job_id.as_str(),
        }
    }

    /// The jobs the scheduler is deciding about for the compute environment `environment_arn`
    /// at `now`, in the order it evaluates them.
    ///
    /// A candidate is a job waiting in `RUNNABLE` that this environment would take
    /// ([`Self::placement_environment`]), that holds no capacity yet, whose own placement has
    /// fallen due at `now` — a job still counting down to its placement, or resting because
    /// its queue offers it nothing, is not competing for capacity yet — and whose consumable
    /// resources have the quantity it claims ([`consumable_claims`]), because a job that
    /// cannot be admitted for another reason must not hold capacity back from the jobs behind
    /// it.
    ///
    /// Costs one walk of the jobs map ([`limits::MAX_JOBS`]), one usage reading per
    /// (queue, share) the candidates name, one key per candidate and one sort of those keys.
    fn dispatch_candidates(&self, environment_arn: &str, now: i64) -> Vec<&Job> {
        // Where each job sits in its queue's SUBMISSION order, which is the order
        // [`Self::jobs_by_queue`] holds a queue's jobs in — read once for the whole sort
        // rather than searched for per comparison.
        let position_of: HashMap<&str, usize> = self
            .jobs_by_queue
            .values()
            .flat_map(|ids| ids.iter().enumerate())
            .map(|(position, id)| (id.as_str(), position))
            .collect();
        let waiting: Vec<&Job> = self
            .jobs
            .values()
            .filter(|job| job.status == JOB_RUNNABLE && !job.holds_compute_capacity())
            .filter(|job| job.next_phase_due_at.is_some_and(|due| due <= now))
            .filter(|job| {
                self.placement_environment(&job.job_id)
                    .is_some_and(|environment| environment.arn == environment_arn)
            })
            .filter(|job| self.consumable_claims_admit(&job.job_id))
            .collect();
        // What each SHARE of each queue has used, read once however many of its jobs are
        // waiting: the usage is a property of the share, not of the candidate.
        let mut used_by_share: HashMap<(&str, Option<&str>), i32> = HashMap::new();
        let mut candidates: Vec<(DispatchKey<'_>, &Job)> = Vec::with_capacity(waiting.len());
        for job in waiting {
            let used = self.share_usage_cached(job, now, &mut used_by_share);
            candidates.push((self.dispatch_key(job, used, &position_of), job));
        }
        candidates.sort_by(|(left, _), (right, _)| left.compare(right));
        candidates.into_iter().map(|(_, job)| job).collect()
    }

    /// Whether the capacity behind `id`'s queue can hold the job at `now` — and whether it is
    /// `id`'s turn to be given it.
    ///
    /// The free capacity is the environment's maximum less what the jobs placed on it hold,
    /// and it is offered to the candidates in the order the scheduler evaluates them
    /// ([`Self::dispatch_candidates`]): each one that fits takes what it reserves, and a
    /// candidate that does not fit what is left is passed over — so `id` is admitted only
    /// when the jobs ahead of it have left room for what it reserves. An environment that
    /// publishes no maximum ([`ComputeEnvironment::vcpu_ceiling`]) holds nobody back.
    ///
    /// A pure read: it decides from the stored records alone, so two callers at one instant
    /// get one answer and a job passed over keeps waiting where a job waits to be placed.
    pub(super) fn capacity_admits(&self, id: &str, now: i64) -> bool {
        let Some(environment) = self.placement_environment(id) else {
            return false;
        };
        let Some(ceiling) = environment.vcpu_ceiling() else {
            return true;
        };
        let environment_arn = environment.arn.clone();
        let mut room = ceiling.saturating_sub(self.vcpus_held_on(&environment_arn));
        for candidate in self.dispatch_candidates(&environment_arn, now) {
            let reserved = job_vcpus(candidate);
            let fits = reserved <= room;
            if candidate.job_id == id {
                return fits;
            }
            if fits {
                room = room.saturating_sub(reserved);
            }
        }
        // A job that is not among the candidates is not competing for this capacity at `now`,
        // so nothing admits it in this pass.
        false
    }
}
