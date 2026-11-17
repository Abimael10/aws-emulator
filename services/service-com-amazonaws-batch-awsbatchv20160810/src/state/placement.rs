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
//! What a placed job can report of the RUN is where the engine contract binds
//! (`vendor/engine/src/lib.rs`): containers are not available in-process, so the attempt a
//! placement starts never gets a container, and the job ends in the modelled failure state
//! saying so — unless the fetch of its container image already ended it ([`image_pull`]).
//! The attempt therefore carries the reason it ended, the instant the task it was made for
//! stopped, and nothing a run would have produced: no `exitCode` ("the exit code for the job
//! attempt") and no `startedAt` (the `STARTING` -> `RUNNING` transition).
//! It does carry the log stream its container's output would have gone to, which the
//! placement already decides ([`job_log_stream_name`]) — and that stream is MATERIALISED
//! where CloudWatch Logs holds it, so a client can list, read or delete it
//! ([`job_logs`]).

use super::*;

/// `statusReason` of a job the scheduler placed and whose container nothing ran.
///
/// A placed job's work is a container image, which this emulator has no runtime for and
/// the engine contract (`vendor/engine/src/lib.rs`) forbids pretending to run: so the run
/// the caller asked for ends in the modelled failure state saying exactly that, and no
/// `startedAt` or `exitCode` is invented for it. It names the condition that
/// stopped the run and nothing from the job.
pub const NO_CONTAINER_RUNTIME_REASON: &str =
    "This emulator does not run containers, so the job's container was never started";

/// What the container initiation a placement started comes to, as the job's `statusReason`:
/// the failure of the image fetch that initiation begins with ([`image_pull`]), or — for an
/// image a registry served — [`NO_CONTAINER_RUNTIME_REASON`], the one end a container of this
/// emulator can reach. ONE reading, so the reason the job ends with and the reason its attempt
/// ends with cannot disagree.
pub fn placement_end_reason(fetch: &ImageFetch) -> String {
    match fetch {
        ImageFetch::Served => NO_CONTAINER_RUNTIME_REASON.to_string(),
        ImageFetch::Refused(reason) => reason.clone(),
    }
}

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
    /// transitioned from the `STARTING` state to the `RUNNING` state)". `None` for an
    /// attempt whose container never ran.
    pub started_at: Option<i64>,
    /// `AttemptDetail$stoppedAt`: "when the attempt was stopped (when the attempt
    /// transitioned from the `RUNNING` state to a terminal state)" — the instant the task this
    /// attempt was made for stopped, written when the attempt ends ([`end_open_attempt`]) and
    /// `None` while it is still open.
    pub stopped_at: Option<i64>,
    /// `AttemptContainerDetail$exitCode`: "the exit code for the job attempt. A non-zero
    /// exit code is considered failed". `None` until a container of this attempt exits.
    pub exit_code: Option<i32>,
    /// `AttemptContainerDetail$logStreamName`: the CloudWatch Logs stream the attempt's
    /// container writes its output to, named by the placement ([`job_log_stream_name`]).
    pub log_stream_name: Option<String>,
    /// `AttemptDetail$statusReason` — "additional details for the current status of the job
    /// attempt" — which is also the attempt container's `reason` ("additional details for a
    /// running or stopped container"). `None` while the attempt is still open.
    pub status_reason: Option<String>,
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
                network_interfaces: None,
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
/// middle segment of the log stream the `awslogs` driver writes that container to.
const LOG_STREAM_CONTAINER_NAME: &str = "default";

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

/// The `detail` rendering of one attempt for a `Batch Job State Change` event, in the
/// member names `AttemptDetail` documents — the SAME members the `DescribeJobs` projection
/// reports, so the two cannot disagree about an attempt.
pub(super) fn attempt_event_detail(attempt: &JobAttempt) -> ::serde_json::Value {
    use ::serde_json::{json, Map, Value};

    let mut container = Map::new();
    container.insert("taskArn".to_string(), json!(attempt.task_arn));
    if let Some(code) = attempt.exit_code {
        container.insert("exitCode".to_string(), json!(code));
    }
    if let Some(reason) = &attempt.status_reason {
        container.insert("reason".to_string(), json!(reason));
    }
    if let Some(name) = &attempt.log_stream_name {
        container.insert("logStreamName".to_string(), json!(name));
    }
    let mut rendered = Map::new();
    rendered.insert("container".to_string(), Value::Object(container));
    if let Some(at) = attempt.started_at {
        rendered.insert("startedAt".to_string(), json!(at));
    }
    if let Some(at) = attempt.stopped_at {
        rendered.insert("stoppedAt".to_string(), json!(at));
    }
    if let Some(reason) = &attempt.status_reason {
        rendered.insert("statusReason".to_string(), json!(reason));
    }
    Value::Object(rendered)
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

    /// PLACE `id` on the capacity its queue offers, recording the job attempt the placement
    /// starts, and answer whether it was placed at all.
    ///
    /// A placement is what makes a task: the attempt is recorded carrying the ARN of the
    /// task this job was placed as ("each container attempt receives a task ARN when they
    /// reach the `STARTING` status"), named under the Amazon ECS cluster backing the compute
    /// environment that took it ([`ecs_cluster`]), and the log stream that task's container
    /// writes to ([`job_log_stream_name`], created where CloudWatch Logs holds it by
    /// [`job_logs`]); the job's own container reports the same two — `ContainerDetail$taskArn`
    /// is "the Amazon Resource Name (ARN) of the Amazon ECS task that's associated with the
    /// container job".
    ///
    /// What that placement then runs into is the answer the registry gave for the container's
    /// image, which the job already carries by the time it is placed ([`image_pull`]): an
    /// image the registry would not serve stops the initiation before any container is
    /// created, while one that is there to be fetched gets the container that
    /// [`NO_CONTAINER_RUNTIME_REASON`] says nothing here can run ([`placement_end_reason`]).
    /// Either way the task was made, so the stream its container's output would have gone to
    /// is named and held — empty, because nothing ran — and nothing about a RUN is recorded:
    /// the attempt has no exit code and no start instant, because nothing reached those
    /// statuses.
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
        let definition = self
            .jobs
            .get(id)
            .map(|job| job.job_definition_arn.clone())
            .unwrap_or_default();
        let task_id = self.new_uuid().replace('-', "");
        let task_arn = ecs_task_arn(&region, &cluster, &task_id);
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
            &task_id,
        );
        let target = job_log_target(configuration.as_ref(), &log_stream_name, &region);
        let reported_stream = target.as_ref().map(|target| target.log_stream.clone());
        if let Some(job) = self.jobs.get_mut(id) {
            job.attempts.push(JobAttempt {
                task_arn: task_arn.clone(),
                started_at: None,
                stopped_at: None,
                exit_code: None,
                log_stream_name: reported_stream.clone(),
                status_reason: None,
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
        true
    }

    /// Move the job the placement just took out of its queue and into `STARTING` at `at`,
    /// owing its end one span later.
    ///
    /// `JobStatus$STARTING` is where "these jobs have been scheduled to a host and the
    /// relevant container initiation operations are underway", which is exactly what a
    /// placement has started: the task exists, the logger its container's output goes to is
    /// initialised, and the image the container runs is fetched. So a placed job is reported
    /// there — never in `RUNNABLE`, which it has left: it is out of its queue's ordering, and
    /// the jobs behind it move forward at this instant — until the end that initiation comes
    /// to falls due, one span later like every other transition this service owes.
    pub(super) fn start_placed_job(&mut self, id: &str, at: i64) {
        self.advance_queue_positions(id, at);
        if let Some(job) = self.jobs.get_mut(id) {
            job.at_position_since = None;
        }
        self.set_job_status(id, JOB_STARTING, at);
        self.set_job_due(id, Some(at + JOB_PHASE_MILLIS));
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
