// SPDX-License-Identifier: CC0-1.0

//! Where a `RUNNABLE` job is PLACED, and what a placed job can report.
//!
//! `JobStatus$RUNNABLE` is the status in which "the job is ready to be scheduled to a
//! host", so what a job does next is decided by the compute capacity behind its queue and
//! not by the job: AWS Batch places a `RUNNABLE` job on a compute environment of its queue
//! that can hold it, and a queue whose environments offer the service no capacity to place
//! it on leaves the job waiting there (`JobDetail$status` points at "Jobs stuck in RUNNABLE
//! status" for exactly that case). `computeResources` is "details of the compute resources
//! managed by the compute environment" (`ComputeEnvironmentDetail$computeResources`), so a
//! `MANAGED` environment is the one whose capacity the SERVICE provisions and schedules
//! onto; an `UNMANAGED` one holds whatever instances the caller registered itself, of which
//! this emulator holds none. `CEState$DISABLED`: "the compute environment doesn't scale out"
//! and Batch "doesn't place jobs" on it, and an environment still `CREATING` has no capacity
//! yet.
//!
//! A placement STARTS A JOB ATTEMPT, and the attempt is what reports the identity of the
//! task the job was placed as: `AttemptContainerDetail$taskArn` is "the Amazon Resource Name (ARN)
//! of the Amazon ECS task that's associated with the job attempt", and "each container
//! attempt receives a task ARN when they reach the `STARTING` status" — so every placement
//! records one [`JobAttempt`] carrying that ARN, and `JobDetail$attempts` ("a list of job
//! attempts that are associated with this job") reports them in order. A job nothing has
//! placed has had no attempt, so the list it reports is EMPTY — present, because an empty
//! list is what "nothing has run yet" looks like, and a missing member is not that answer.
//!
//! What a placed job reports OF THE RUN is decided by the CAPACITY it was placed on. Capacity
//! that can take an Amazon ECS task runs the container as one ([`ecs_task`]), and everything
//! the attempt then reports — the instant its container started, the instant the task stopped,
//! the status that container exited with and the elastic network interface the task is reached
//! on — is read off that task. Capacity that can take none performs the program the container
//! declares in this process instead ([`container_run`]), so the attempt reports the instant
//! that program started, the status it exited with and the lines it wrote. Either way an
//! attempt whose container never ran — a program nothing performed, an image the registry
//! would not serve ([`image_pull`]) — reports none of those, because nothing ran.
//! It does carry the log stream its container's output goes to, which the
//! placement already decides ([`job_log_stream_name`]) — and that stream is MATERIALISED
//! where CloudWatch Logs holds it, so a client can list, read or delete it
//! ([`job_logs`]).

use super::*;

/// ONE job attempt: what the scheduler placing a job's container started, and what the
/// attempt is known to have produced.
///
/// The attempt exists from the placement, so [`Self::task_arn`] is always there — it is the
/// task the placement made — while every member that describes the RUN is filled only once
/// the attempt's container actually got that far, and stays absent otherwise.
#[derive(Debug, Clone)]
pub struct JobAttempt {
    /// `AttemptContainerDetail$taskArn`: "the Amazon Resource Name (ARN) of the Amazon ECS task that's
    /// associated with the job attempt", minted once by the placement that started this
    /// attempt ([`ecs_task_arn`]).
    pub task_arn: String,
    /// `AttemptDetail$startedAt`: "when the attempt was started (when the attempt
    /// transitioned from the `STARTING` state to the `RUNNING` state)" — written when the
    /// container's own program starts ([`container_run`]). `None` for an attempt whose
    /// container never ran.
    pub started_at: Option<i64>,
    /// `AttemptDetail$stoppedAt`: "when the attempt was stopped (when the attempt
    /// transitioned from the `RUNNING` state to a terminal state)" — the instant the task this
    /// attempt was made for stopped, written when the attempt ends ([`end_open_attempt`]) and
    /// `None` while it is still open.
    pub stopped_at: Option<i64>,
    /// `AttemptContainerDetail$exitCode`: "the exit code for the job attempt. A non-zero
    /// exit code is considered failed" — the status the container's program exited with.
    /// `None` until a container of this attempt exits.
    pub exit_code: Option<i32>,
    /// `AttemptContainerDetail$logStreamName`: the CloudWatch Logs stream the attempt's
    /// container writes its output to, named by the placement ([`job_log_stream_name`]).
    pub log_stream_name: Option<String>,
    /// `AttemptDetail$statusReason` — "additional details for the current status of the job
    /// attempt" — which is also the attempt container's `reason` ("additional details for a
    /// running or stopped container"). `None` while the attempt is still open.
    pub status_reason: Option<String>,
    /// `AttemptContainerDetail$networkInterfaces`: "the network interfaces associated with
    /// the container", which is the elastic network interface the task this attempt was made
    /// for is reached on — read off that task ([`ecs_task`]) and empty for an attempt whose
    /// task holds none.
    pub network_interfaces: Vec<NetworkInterface>,
    /// When this attempt ENDED, in epoch milliseconds, or `None` while it is still open.
    ///
    /// Internal bookkeeping, not a modelled member: it is what tells the attempt a job's
    /// ending must close ([`end_open_attempt`]) from the attempts that already closed. It
    /// is NOT `stoppedAt`, which AWS Batch documents as the `RUNNING` -> terminal
    /// transition and so leaves absent for an attempt whose container never ran.
    pub ended_at: Option<i64>,
}

impl JobAttempt {
    /// This attempt as the modelled `AttemptDetail` every projection reports it as.
    ///
    /// Read straight off the record, so the attempt a reader sees cannot disagree with the
    /// placement that made it, and a member the record does not carry is left ABSENT rather
    /// than filled with a value nothing produced.
    pub fn detail(&self) -> AttemptDetail {
        AttemptDetail {
            container: Some(AttemptContainerDetail {
                // No container instance hosts the attempt: this store holds none, and a
                // Fargate task has no container instance at all.
                container_instance_arn: None,
                task_arn: Some(self.task_arn.clone()),
                exit_code: self.exit_code,
                reason: self.status_reason.clone(),
                log_stream_name: self.log_stream_name.clone(),
                // Materialised like every other collection of a described job: an attempt
                // whose task holds no interface reports the EMPTY list, which is what
                // "nothing is attached" looks like, rather than a missing member.
                network_interfaces: Some(self.network_interfaces.clone()),
            }),
            started_at: self.started_at,
            stopped_at: self.stopped_at,
            status_reason: self.status_reason.clone(),
            // `taskProperties` describes "a task definition that describes the container and
            // volume definitions of an Amazon ECS task", which only a job submitted with
            // `ecsProperties` has; nothing here stores those.
            task_properties: None,
        }
    }
}

/// `arn:aws:ecs:<region>:<account>:task/<cluster>/<task id>` — the ECS task a placement
/// made, named under the compute environment whose capacity the job was placed on, which is
/// the only cluster this service knows the task by.
///
/// The id is the 32 lowercase hex characters an ECS task id is, taken from the same minted
/// UUID every other identifier here comes from ([`new_uuid`]) with its separators removed.
pub fn ecs_task_arn(region: &str, cluster: &str, task_id: &str) -> String {
    ::engine::emulator_arn("ecs", region, &format!("task/{cluster}/{task_id}"))
}

/// The region field of an ARN this service minted, or `None` when `value` carries none.
pub(super) fn arn_region(value: &str) -> Option<&str> {
    value
        .starts_with("arn:")
        .then(|| value.split(':').nth(3))
        .flatten()
        .filter(|region| !region.is_empty())
}

/// The container name AWS Batch gives the single container of a container job, and so the
/// middle segment of the log stream the `awslogs` driver writes that container to — and the
/// name of the one container of the Amazon ECS task the job runs as ([`ecs_task`]).
pub(super) const LOG_STREAM_CONTAINER_NAME: &str = "default";

/// The CloudWatch Logs stream a placed job's container writes to:
/// `<stream prefix>/default/<ECS task id>`, in the log group the driver delivers to
/// ([`job_logs`], whose default is `/aws/batch/job` — "the log group for Batch jobs is
/// `/aws/batch/job`", `AttemptContainerDetail$logStreamName`).
///
/// The three segments are the `awslogs` driver's own naming: the `awslogs-stream-prefix` the
/// container's log configuration carries ([`job_log_stream_prefix`], which for a Batch job
/// that configures none is the job definition's name), the name of the container the output
/// belongs to, and the id of the task it ran as. The placement decides every one of them, and
/// AWS is recorded to report the name for an attempt whose container never ran — it comes with
/// the task, not with the run — so it is written at the placement. The output itself is another
/// matter: a container nothing ran wrote none, so no event exists in that stream.
pub fn job_log_stream_name(stream_prefix: &str, task_id: &str) -> String {
    format!("{stream_prefix}/{LOG_STREAM_CONTAINER_NAME}/{task_id}")
}

/// End the attempt `job` still has open, if any, at `at` with `reason`.
///
/// An attempt lasts as long as the job can still be running it, so the ending of the job is
/// the ending of its open attempt, with the same reason: whatever took the job terminal —
/// the clock, `CancelJob`, `TerminateJob`, the deletion of its queue — ended the attempt
/// too. An attempt that already closed is left exactly as it closed, which is what keeps a
/// job's earlier attempts reporting what each of them ended with.
pub(super) fn end_open_attempt(job: &mut Job, reason: Option<&str>, at: i64) {
    let Some(attempt) = job
        .attempts
        .last_mut()
        .filter(|attempt| attempt.ended_at.is_none())
    else {
        return;
    };
    attempt.ended_at = Some(at);
    attempt.status_reason = reason.map(str::to_string);
    // The attempt exists because a task was made for it, and that task stopped at this
    // instant, so the attempt reports its stop whatever its container got to — the same
    // reading the job's own `stoppedAt` takes.
    attempt.stopped_at = Some(at);
}

/// The `detail` rendering of one attempt for a `Batch Job State Change` event: the SAME
/// `AttemptDetail` document the `DescribeJobs` projection reports for it ([`Self::detail`]),
/// walked through the shape's own schema
/// ([`::service_common::management_events::recorded_shape`]) — AWS documents the event's
/// detail as the job's details "in the same form as the `DescribeJobs` API returns them", and
/// ONE rendering is what keeps the two from disagreeing about an attempt, or from one of them
/// forgetting a member the other reports.
pub(super) fn attempt_event_detail(attempt: &JobAttempt) -> ::serde_json::Value {
    ::service_common::management_events::recorded_shape(
        &attempt.detail(),
        &AttemptDetail::SCHEMA,
        ::service_common::management_events::WithheldFromRecord::default(),
    )
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The compute environment of `id`'s queue the scheduler would place the job on, or
    /// `None` when the queue offers it no capacity at all.
    ///
    /// A candidate is a `MANAGED` compute environment that is `ENABLED` and past its own
    /// creation — the environment whose capacity the service itself provisions and schedules
    /// onto. An `UNMANAGED` environment (whose instances the caller registers, of which this
    /// store holds none), a `DISABLED` one (Batch places no job on it) and one still
    /// `CREATING` or `DELETING` offer none, so a queue with only those leaves its jobs
    /// waiting in `RUNNABLE`. Of the candidates, the queue's own order decides:
    /// "compute environments are tried in ascending order ... the compute environment with a
    /// lower order integer value is tried for job placement first"
    /// (`ComputeEnvironmentOrder$order`).
    ///
    /// An array PARENT is never placed: it runs no container of its own, and its status is
    /// what its children add up to ([`Self::advance_array_parents`]).
    ///
    /// Costs one walk of the queue's `computeEnvironmentOrder`, which
    /// `CreateJobQueue`/`UpdateJobQueue` bound, and reads nothing else.
    pub fn placement_environment(&self, id: &str) -> Option<&ComputeEnvironment> {
        let job = self.jobs.get(id)?;
        if job.array_size.is_some() {
            return None;
        }
        let queue = self.job_queue(&job.job_queue_arn)?;
        queue
            .compute_environment_order
            .iter()
            .filter_map(|entry| {
                let environment = self.compute_environment(&entry.compute_environment)?;
                let offers_capacity = environment.ce_type == CE_TYPE_MANAGED
                    && environment.state == STATE_ENABLED
                    && matches!(environment.status.as_str(), STATUS_VALID | STATUS_UPDATING);
                offers_capacity.then_some((entry.order, environment))
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, environment)| environment)
    }

    /// Whether the compute environments behind `id`'s queue offer the service capacity to
    /// place the job on ([`Self::placement_environment`]).
    pub fn job_is_placeable(&self, id: &str) -> bool {
        self.placement_environment(id).is_some()
    }

    /// PLACE `id` on the capacity its queue offers under a task this service NAMES itself, and
    /// answer whether it was placed at all.
    ///
    /// The placement of a job whose container an Amazon ECS task RUNS is made by that task
    /// ([`ecs_task`], which records the placement through [`Self::record_placement`] with the
    /// ARN that service answered). This is the placement of every other one: capacity no task
    /// could be placed on, and a run nothing served. The attempt is recorded carrying the ARN
    /// of the task this job was placed as ("each container attempt receives a task ARN when
    /// they reach the `STARTING` status"), named under the Amazon ECS cluster backing the
    /// compute environment that took it ([`ecs_cluster`]), and the log stream that task's
    /// container writes to ([`job_log_stream_name`], created where CloudWatch Logs holds it by
    /// [`job_logs`]); the job's own container reports the same two — `ContainerDetail$taskArn`
    /// is "the Amazon Resource Name (ARN) of the Amazon ECS task that's associated with the
    /// container job".
    ///
    /// What that placement then runs into is the answer the registry gave for the container's
    /// image, which the job already carries by the time it is placed ([`image_pull`]): an
    /// image the registry would not serve stops the initiation before any container is
    /// created, while one that is there to be fetched gets the container that runs the
    /// program the job declared — as the Amazon ECS task the capacity takes ([`ecs_task`]), or
    /// performed in this process where it can take none ([`container_run`]).
    /// Either way the task was made, so the stream its container's output goes to
    /// is named and held, and nothing about a RUN is recorded by the placement itself:
    /// the attempt has no exit code and no start instant until the program it runs reaches
    /// those statuses.
    ///
    /// A queue with no capacity places nothing, so a job resting in `RUNNABLE` records no
    /// attempt — there has been none.
    pub(super) fn place_job(&mut self, id: &str) -> bool {
        let Some(cluster) = self.placement_environment(id).map(|environment| {
            environment
                .ecs_cluster_name
                .clone()
                .unwrap_or_else(|| environment.name.clone())
        }) else {
            return false;
        };
        // Every ARN this service mints carries the region of the request that created the
        // resource, which for a task placed by the clock is the region of the job itself.
        let region = self
            .jobs
            .get(id)
            .and_then(|job| arn_region(&job.job_arn))
            .unwrap_or(DEFAULT_REGION)
            .to_string();
        let task_id = self.new_uuid().replace('-', "");
        let task_arn = ecs_task_arn(&region, &cluster, &task_id);
        self.record_placement(id, task_arn, &task_id, &region);
        true
    }

    /// Record the placement of `id` as the task `task_arn`: the attempt it starts, the stream
    /// that task's container writes to, and the same two on the job's own container.
    ///
    /// The ONE place a placement is written, whether this service named the task itself
    /// ([`Self::place_job`]) or Amazon ECS answered with the task it created for the job
    /// ([`ecs_task`]) — so an attempt reports the same members however the job was placed.
    pub(super) fn record_placement(
        &mut self,
        id: &str,
        task_arn: String,
        task_id: &str,
        region: &str,
    ) {
        let definition = self
            .jobs
            .get(id)
            .map(|job| job.job_definition_arn.clone())
            .unwrap_or_default();
        // WHERE that stream is — and whether there is a CloudWatch Logs stream for this
        // container at all — is decided by the log driver the job's container configures
        // ([`job_log_target`]), so a container whose output goes to another driver reports
        // no CloudWatch Logs stream and has none created for it.
        let configuration = self
            .jobs
            .get(id)
            .and_then(|job| job.container.as_ref())
            .and_then(|container| container.log_configuration.clone());
        let log_stream_name = job_log_stream_name(
            &job_log_stream_prefix(configuration.as_ref(), &definition),
            task_id,
        );
        let target = job_log_target(configuration.as_ref(), &log_stream_name, region);
        let reported_stream = target.as_ref().map(|target| target.log_stream.clone());
        if let Some(job) = self.jobs.get_mut(id) {
            job.attempts.push(JobAttempt {
                task_arn: task_arn.clone(),
                started_at: None,
                stopped_at: None,
                exit_code: None,
                log_stream_name: reported_stream.clone(),
                status_reason: None,
                // Nothing has read the interface of the task this placement made yet: it is
                // reported off that task ([`ecs_task`]) once the task reports one.
                network_interfaces: Vec::new(),
                ended_at: None,
            });
            if let Some(container) = job.container.as_mut() {
                container.task_arn = Some(task_arn);
                container.log_stream_name = reported_stream;
            }
        }
        // Naming the stream is not holding it: the stream this placement named is created
        // where CloudWatch Logs holds it, by the caller that drains this queue once the
        // store's lock is released ([`job_logs`]). It is opened for the TASK, so it exists
        // whatever the container then did — including a container the image fetch stopped
        // from being created, which AWS is recorded to leave exactly this one empty stream
        // behind. What it holds is another matter: nothing ran, so no event is in it.
        if let Some(target) = target {
            self.queue_log_stream(target);
        }
    }

    /// Move the job the placement just took out of its queue and into `STARTING` at `at`,
    /// owing its next step one span later.
    ///
    /// `JobStatus$STARTING` is where "these jobs have been scheduled to a host and the
    /// relevant container initiation operations are underway", which is exactly what a
    /// placement has started: the task exists, the logger its container's output goes to is
    /// initialised, and the image the container runs is fetched. So a placed job is reported
    /// there — never in `RUNNABLE`, which it has left: it is out of its queue's ordering, and
    /// the jobs behind it move forward at this instant — until what the capacity does with
    /// that container happens, one span later like every other transition this service owes:
    /// the next READ of the Amazon ECS task the job runs as ([`ecs_task`]), or the start of
    /// the program this service performs for a job no task ran ([`container_run`]).
    pub(super) fn start_placed_job(&mut self, id: &str, at: i64) {
        self.advance_queue_positions(id, at);
        if let Some(job) = self.jobs.get_mut(id) {
            job.at_position_since = None;
        }
        self.set_job_status(id, JOB_STARTING, at);
        // One span, or the instant an attempt already running runs out of the time its own
        // `timeout` allows it, whichever falls first ([`attempt_timeout`]) — the same bound
        // every other site that arms a placed job's next instant applies.
        self.set_job_due_bounded_by_attempt_deadline(id, at.saturating_add(JOB_PHASE_MILLIS));
    }

    /// Owe `id` its placement, or nothing at all when its queue has no capacity to place it
    /// on — in which case the job RESTS in `RUNNABLE`.
    ///
    /// WHEN that placement falls due is the capacity's to decide: one span for capacity that
    /// is already running, later for a compute environment that has to provision instances
    /// for the job first ([`Self::placement_due_at`]).
    ///
    /// Called from the ONE place a job enters `RUNNABLE`, so the decision is taken once
    /// per entry and from the instant the job entered, never from the instant a reader
    /// looked.
    pub(super) fn arm_placement(&mut self, id: &str, at: i64) {
        let due = self.placement_due_at(id, at);
        self.set_job_due(id, due);
    }

    /// Owe the placement span again to every job resting in `RUNNABLE` whose queue the
    /// capacity published at `at` now lets it be placed on.
    ///
    /// A resting job owes nothing while its queue's compute environments offer no capacity,
    /// so what makes its next move due is the change that publishes some: a compute
    /// environment reaching the end of its creation, one a caller re-enabled
    /// (`UpdateComputeEnvironment`), or a queue given a different `computeEnvironmentOrder`
    /// (`UpdateJobQueue`). The placement then falls due as the capacity behind the queue
    /// decides ([`Self::placement_due_at`]) — counted from `at`, the instant the capacity was
    /// published, so what a later caller sees does not depend on when it called. The twin
    /// of [`Self::rearm_resting_service_jobs`] for container jobs.
    pub fn rearm_resting_jobs(&mut self, at: i64) {
        let resting: Vec<String> = self
            .jobs
            .values()
            .filter(|job| job.status == JOB_RUNNABLE && job.next_phase_due_at.is_none())
            .map(|job| job.job_id.clone())
            .filter(|id| self.job_is_placeable(id))
            .collect();
        for id in resting {
            let due = self.placement_due_at(&id, at);
            self.set_job_due(&id, due);
        }
    }
}
