// SPDX-License-Identifier: CC0-1.0

//! The capacity a MANAGED compute environment has to PROVISION before a job waiting on one
//! of its queues can be placed, and what that provisioning is reported as.
//!
//! `JobStatus$RUNNABLE` is where "the job is ready to be scheduled to a host": the job has
//! been accepted, nothing is left for it to wait for, and what it is waiting on is the
//! capacity of a compute environment behind its queue. Whether that capacity is there YET is
//! what the compute type decides. `ComputeResource$minvCpus` is "the minimum number of vCPUs
//! that a compute environment should maintain", so an environment with a `minvCpus` above
//! zero is already running instances a job can be placed on; one maintaining NONE has to
//! launch an instance first, and `ComputeResource$desiredvCpus` — "the desired number of
//! vCPUS in the compute environment. Batch modifies this value between the minimum and
//! maximum values based on job queue demand" — is the service's own record of the capacity it
//! has scaled out to for the jobs its queues are asking for. Fargate capacity, by contrast,
//! is no instance of this account's that Batch launches, so nothing has to be provisioned
//! before a job placed on it can run.
//!
//! So a job submitted to a queue whose only capacity is an EC2 compute environment
//! maintaining no vCPUs WAITS in `RUNNABLE` while that environment scales out — which is
//! exactly the state `GetJobQueueSnapshot` reports ("the Amazon Resource Names (ARNs) of the
//! first 100 `RUNNABLE` jobs in a named job queue"), so a client polling that snapshot
//! observes the outstanding job while the capacity for it is being provisioned, and
//! `DescribeComputeEnvironments` reports the `desiredvCpus` that demand raised. A job on an
//! environment that already maintains capacity is placed one span after the scheduler looked
//! at it, as is every job on a Fargate environment.
//!
//! Nothing here reports a resource of another service: the capacity is this service's own
//! record of what it scaled out to, which is the member AWS Batch publishes for it.

use super::*;

/// How long ONE step of provisioning the Amazon EC2 capacity a queue's jobs need takes.
///
/// A span chosen by this emulator: AWS documents no duration for launching an instance into a
/// compute environment, where the real wait is minutes. It is long enough that a client
/// polling the queue's snapshot observes the job waiting for capacity — the observation AWS
/// Batch publishes that snapshot for — and short enough that the job runs immediately
/// afterwards.
pub const CAPACITY_STEP_MILLIS: i64 = 3_000;

/// How many of those steps provisioning capacity from nothing takes before the scheduler can
/// place a job on it: the instance has to be LAUNCHED, and it has to JOIN the Amazon ECS
/// cluster that backs the environment ("determine the `ecsClusterArn` that you launch your
/// Amazon ECS container instances into", `DescribeComputeEnvironments`) before a task can be
/// placed on it. Each step is one span, so an environment that maintains no capacity costs
/// both.
pub const CAPACITY_STEPS: i64 = 2;

/// The vCPUs a job is taken to demand when its container states none this service can read.
///
/// `ResourceRequirement$value` for `type` `VCPU` is "the number of vCPUs reserved for the
/// container", and a container job runs on at least one, so a job that states no vCPU
/// requirement at all still demands capacity rather than none.
const MIN_JOB_VCPUS: i32 = 1;

impl ComputeEnvironment {
    /// Whether the capacity behind this environment is Amazon EC2 instances AWS Batch
    /// launches for it.
    ///
    /// `computeResources` is "details about the compute resources managed by the compute
    /// environment. This parameter is required for managed compute environments", so only a
    /// MANAGED environment has capacity this service provisions at all — an `UNMANAGED` one
    /// holds whatever instances the caller registered itself — and of the managed ones the
    /// Fargate compute types ([`is_fargate_compute_type`]) need no instance of the account's
    /// own launched for them.
    pub fn provisions_instances(&self) -> bool {
        self.ce_type == CE_TYPE_MANAGED
            && self
                .compute_resources
                .as_ref()
                .is_some_and(|resources| !is_fargate_compute_type(&resources.r#type))
    }

    /// The vCPUs this environment MAINTAINS whatever its queues are asking for
    /// (`ComputeResource$minvCpus`), which is capacity a job can be placed on straight away.
    fn maintained_vcpus(&self) -> i32 {
        self.compute_resources
            .as_ref()
            .and_then(|resources| resources.minv_cpus)
            .unwrap_or_default()
            .max(0)
    }

    /// Record the capacity this environment is scaling out to for a demand of `vcpus` at `at`,
    /// and answer the earliest instant a job the scheduler looked at at `at` can be placed on
    /// it.
    ///
    /// `desiredvCpus` is what the service has scaled out to, "between the minimum and the
    /// maximum values based on job queue demand", so the demand is held between `minvCpus` and
    /// `maxvCpus` ("the maximum number of vCPUs that a compute environment can support"). A
    /// demand of nothing scales the environment back to the vCPUs it maintains, and the
    /// capacity it had launched for that demand goes with it — so the next job to arrive waits
    /// for an instance again, exactly as the first one did.
    fn scale_to(&mut self, vcpus: i32, at: i64) -> i64 {
        let one_span = at + JOB_PHASE_MILLIS;
        let maintained = self.maintained_vcpus();
        if let Some(resources) = self.compute_resources.as_mut() {
            let ceiling = resources.maxv_cpus.max(maintained);
            resources.desiredv_cpus = Some(vcpus.clamp(maintained, ceiling));
        }
        if vcpus <= 0 {
            self.capacity_ready_at = None;
        } else if self.capacity_ready_at.is_none() {
            self.capacity_ready_at = Some(at + CAPACITY_STEPS * CAPACITY_STEP_MILLIS);
        }
        // Instances it maintains are already running, so a job of such an environment waits
        // for nothing but the scheduler; otherwise the placement waits for the capacity this
        // scale-out is launching.
        if maintained > 0 {
            return one_span;
        }
        self.capacity_ready_at.unwrap_or(one_span).max(one_span)
    }
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// When the job `id` can be PLACED, given the capacity the compute environment behind its
    /// queue has — or `None` when that queue offers it no capacity to be placed on at all, in
    /// which case the job rests in `RUNNABLE` ([`Self::placement_environment`]).
    ///
    /// Never earlier than one span after `at`, which is the scheduler's own look at a job that
    /// has just become `RUNNABLE`; later than that exactly when the environment has to
    /// provision capacity for it first ([`ComputeEnvironment::scale_to`]). Called from the
    /// places a job ENTERS `RUNNABLE`, so the answer is computed from the instant the job
    /// entered and never from the instant a reader looked.
    pub(super) fn placement_due_at(&mut self, id: &str, at: i64) -> Option<i64> {
        let environment = self.placement_environment(id)?;
        if !environment.provisions_instances() {
            return Some(at + JOB_PHASE_MILLIS);
        }
        let name = environment.name.clone();
        Some(self.rescale(&name, at).unwrap_or(at + JOB_PHASE_MILLIS))
    }

    /// Scale the compute environment `name` to the demand of the jobs its queues are asking
    /// capacity for, at `at`, and answer when that capacity can hold a job placed now.
    ///
    /// Called where the demand CHANGES — a job entering `RUNNABLE`, a job reaching a terminal
    /// status — so what the environment reports is the demand of the jobs it holds at that
    /// instant and never a figure a read invented.
    fn rescale(&mut self, name: &str, at: i64) -> Option<i64> {
        let demand = self
            .compute_environments
            .get(name)
            .map(|environment| self.vcpu_demand_on(&environment.arn))?;
        self.compute_environments
            .get_mut(name)
            .map(|environment| environment.scale_to(demand, at))
    }

    /// Scale every compute environment the queue `arn` orders to the demand its jobs now make,
    /// at `at`: a job of that queue has stopped needing capacity, so the environments behind
    /// it are holding capacity for one job fewer.
    pub(super) fn rescale_queue_capacity(&mut self, arn: &str, at: i64) {
        let Some(order) = self
            .job_queue(arn)
            .map(|queue| queue.compute_environment_order.clone())
        else {
            return;
        };
        self.rescale_ordered_capacity(&order, at);
    }

    /// The same, for a `computeEnvironmentOrder` held outside the store: the order of a queue
    /// that has just been DELETED, whose jobs went with it and whose environments are
    /// therefore holding capacity for nothing of its any more.
    ///
    /// One scale per ENVIRONMENT, not per entry: the entries are a caller-filled list that
    /// may name one environment more than once, so they are resolved to the environments
    /// they name first — a set the store itself bounds
    /// ([`limits::MAX_COMPUTE_ENVIRONMENTS`]), since an entry only resolves to an
    /// environment this service holds.
    pub(super) fn rescale_ordered_capacity(
        &mut self,
        order: &[ComputeEnvironmentOrder],
        at: i64,
    ) {
        let ordered: BTreeSet<String> = order
            .iter()
            .filter_map(|entry| self.compute_environment(&entry.compute_environment))
            .filter(|environment| environment.provisions_instances())
            .map(|environment| environment.name.clone())
            .collect();
        for name in ordered {
            self.rescale(&name, at);
        }
    }

    /// The vCPUs the jobs that still need capacity from the compute environment
    /// `environment_arn` are asking for, summed.
    ///
    /// A job needs capacity from the instant it is accepted until it reaches a terminal
    /// status, and a job of a queue that orders this environment is a job this environment may
    /// have to hold. An array PARENT is not counted: it runs no container of its own, so it
    /// asks for no capacity — its children do ([`Self::advance_array_parents`]).
    ///
    /// Costs one walk of the queues this store holds ([`limits::MAX_JOB_QUEUES`]), each of
    /// whose orders is bounded ([`limits::MAX_COMPUTE_ENVIRONMENT_ORDER_ENTRIES`]), plus one
    /// walk of the jobs on the queues that order this environment, which the jobs store
    /// bounds ([`limits::MAX_JOBS`]) — so what one request costs is decided by what the store
    /// holds and never by the size of a list the request named.
    fn vcpu_demand_on(&self, environment_arn: &str) -> i32 {
        let queues: Vec<&String> = self
            .job_queues
            .values()
            .filter(|queue| {
                queue.compute_environment_order.iter().any(|entry| {
                    self.compute_environment(&entry.compute_environment)
                        .is_some_and(|environment| environment.arn == environment_arn)
                })
            })
            .map(|queue| &queue.arn)
            .collect();
        let mut demand: i32 = 0;
        for queue in queues {
            for job in self
                .jobs_by_queue
                .get(queue)
                .into_iter()
                .flatten()
                .filter_map(|id| self.jobs.get(id))
                .filter(|job| job.array_size.is_none() && !is_terminal_status(&job.status))
            {
                demand = demand.saturating_add(job_vcpus(job));
            }
        }
        demand
    }
}

/// The vCPUs `job`'s container reserves: the `VCPU` entries of its `resourceRequirements`
/// ("the number of vCPUs reserved for the container"), else the `vcpus` member its container
/// carries, and at least [`MIN_JOB_VCPUS`].
///
/// The ONE reading of a job's reservation: the demand a compute environment scales out to
/// here, and the capacity a placed job holds against that environment's maximum
/// ([`super::compute_capacity`]), are the same figure.
///
/// A Fargate job may reserve a FRACTION of a vCPU ("0.25") while the capacity an environment
/// scales out to is whole vCPUs, so a fraction is rounded up: capacity holding a quarter of a
/// vCPU is one vCPU of an instance.
pub(super) fn job_vcpus(job: &Job) -> i32 {
    let Some(container) = job.container.as_ref() else {
        return MIN_JOB_VCPUS;
    };
    let requested: f64 = container
        .resource_requirements
        .iter()
        .flatten()
        .filter(|requirement| requirement.r#type == RESOURCE_TYPE_VCPU)
        .filter_map(|requirement| requirement.value.parse::<f64>().ok())
        .sum();
    let whole = requested.ceil().clamp(0.0, f64::from(i32::MAX));
    let stated = if whole > 0.0 {
        i32::try_from(whole as i64).unwrap_or(i32::MAX)
    } else {
        container.vcpus.unwrap_or_default()
    };
    stated.max(MIN_JOB_VCPUS)
}
