// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECS TASK a placed container job RUNS AS, and what that task reports back.
//!
//! # The rule
//!
//! A Batch job's work is a container, and the container of a job AWS Batch orchestrates with
//! Amazon ECS is a container of an Amazon ECS TASK: `AttemptContainerDetail$taskArn` is "the
//! Amazon Resource Name (ARN) of the Amazon ECS task that's associated with the job attempt",
//! and `ComputeEnvironmentDetail$ecsClusterArn` is "the Amazon Resource Name (ARN) of the
//! underlying Amazon ECS cluster that the compute environment uses". So placing a job on the
//! capacity its queue offers is RUNNING that task in that cluster, and everything the job
//! reports of the run from then on is read off the task Amazon ECS holds for it: the status
//! ladder it walks (`STARTING` while "the relevant container initiation operations are
//! underway", `RUNNING` once its container is, `SUCCEEDED` when that container exits 0 and
//! `FAILED` otherwise), `JobDetail$startedAt`/`stoppedAt`, and the attempt's
//! `exitCode`, `reason`, `logStreamName` and `networkInterfaces`.
//!
//! Nothing here invents any of that. The task is created through Amazon ECS's own API over
//! this emulator's in-process callback — the same hop the cluster behind the environment is
//! created over ([`super::ecs_cluster`]) — so the ARN this service publishes is a task that
//! service's `DescribeTasks` answers for, the container's program is performed where Amazon
//! ECS performs one, and the exit status the job's `SUCCEEDED`/`FAILED` is decided from is the
//! status that program ended with. A job whose container produced no exit status at all ran
//! nothing, and that is what it reports — in the words this service words a program nothing
//! performed with wherever it happens ([`nothing_performed_reason`], over
//! [`super::container_run`]'s own sentences): no status is
//! claimed for work nothing performed.
//!
//! # Which jobs run as a task
//!
//! The ones whose capacity can TAKE one. Fargate capacity is not an instance of this
//! account's that Batch launches ([`super::ec2_capacity`]), so a task of a Fargate compute
//! environment needs no host registered into its cluster first and the cluster can run it the
//! moment the job is placed. An environment whose capacity is Amazon EC2 instances places its
//! tasks on the container instances those instances register into the cluster, and this
//! service launches no instance of the account's into one — so there is no host in that
//! cluster for the task, nothing runs the container, and such a job ends saying exactly that,
//! as it did before anything ran at all. The same holds for an environment this service does
//! not orchestrate with Amazon ECS ([`ComputeEnvironment::orchestrated_with_ecs`]) and for one
//! whose cluster nothing holds.
//!
//! # What the clock owes, and what a call does
//!
//! The clock owes a placed job nothing: from the placement on, what moves the job is the TASK,
//! so each step is a call to Amazon ECS rather than a tick. The job's due instant is what
//! keeps those calls coming — one every [`JOB_TASK_POLL_MILLIS`] while its task is live — and
//! the drain every job-plane operation makes, which the clock pump drives too
//! ([`super::job_clock`]), is what performs them with this store's lock RELEASED. A job whose
//! task has been asked for but not yet created waits in `RUNNABLE`, where a job waits to be
//! placed, so no caller is ever shown an attempt for a task that does not exist.
//!
//! Every call is BOUNDED: at most [`MAX_TASK_STEPS_PER_CALL`] steps per drain, and a dispatch
//! whose answer never came is attempted again once [`TASK_DISPATCH_RETRY_AFTER_MILLIS`] has
//! passed. One registration is made per placement, which the jobs store's own cap
//! ([`limits::MAX_JOBS`]) bounds.
//!
//! An emulator with no Amazon ECS mounted answers the dispatch with the front door's own
//! refusal rather than that service's: the call reached nothing, so no task runs the job's
//! container, and the job takes the placement and the run of a job on capacity that can take
//! no task — its declared program performed in this process ([`super::container_run`]).

use super::*;

use ::aws_sdk_ecs::types as ecs;

use crate::ops::types::NetworkInterface;

/// How often the task a live job is running as is read.
///
/// One span of this service's own clock ([`JOB_PHASE_MILLIS`]): a job running as a task moves
/// when the task does, and the task's own transitions are the other service's spans, so this
/// is how finely this service follows them rather than a duration AWS publishes.
pub const JOB_TASK_POLL_MILLIS: i64 = JOB_PHASE_MILLIS;

/// How long a dispatch already in flight is left alone before it is attempted again.
///
/// A dispatch is marked in flight so two concurrent callers cannot run one job's container
/// twice; this is what keeps a job whose dispatching call never came back from waiting for
/// ever. The twin of [`DISPATCH_RETRY_AFTER_MILLIS`] on the service-job plane.
pub const TASK_DISPATCH_RETRY_AFTER_MILLIS: i64 = 5_000;

/// How many reads of a job's task may say NOTHING before this service gives up on it.
///
/// A read that could not be answered says nothing about the run ([`Outcome::Unread`]), so the
/// job keeps the status it has and the read is made again; this bounds that waiting, because a
/// task Amazon ECS no longer answers for — its cluster deleted under the job, the service
/// unmounted — would otherwise leave the job in a non-terminal status for ever. Several spans
/// of this service's clock, so a read lost to one busy moment costs nothing.
pub const MAX_UNANSWERED_TASK_READS: u32 = 8;

/// How many task steps ONE drain performs.
///
/// The EMULATOR's own bound: each step is a call into this same process, so how many a single
/// request makes is fixed here rather than by how many jobs the store happens to hold. What is
/// left over keeps the clock pump awake and is picked up by the next drain.
pub const MAX_TASK_STEPS_PER_CALL: usize = 8;

/// The `reason` a `StopTask` of the task behind an ENDED job carries: the job was ended by its
/// caller (`CancelJob`, `TerminateJob`) or by the removal of its queue, and the task it was
/// running as is ended with it. It names the condition and nothing of the job.
const TASK_STOPPED_WITH_JOB_REASON: &str = "The Batch job this task was running has ended";

/// How far the Amazon ECS task a container job runs as has got.
///
/// Not a modelled member: the modelled `JobStatus` is what a reader sees, and this is the
/// bookkeeping that decides which step — if any — this service owes Amazon ECS for the job.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JobTaskDispatch {
    /// Nothing is owed: the job has not been placed on capacity that runs a task for it.
    #[default]
    Waiting,
    /// The scheduler placed the job, so its container is owed to Amazon ECS as a task.
    Due,
    /// A dispatch call is out ([`TASK_DISPATCH_RETRY_AFTER_MILLIS`]).
    InFlight,
    /// Amazon ECS holds the task, whose ARN is on the record ([`Job::task_arn`]).
    Running,
    /// Amazon ECS has been asked to stop the task, which is what the ending of a job that was
    /// still running one asks of it.
    Stopped,
}

/// Where a placed job's task is RUN, and what it runs: everything the dispatch needs, read off
/// the job and the compute environment that took it under this store's lock.
#[derive(Debug, Clone)]
pub struct TaskPlacement {
    /// The job the task is run for.
    pub job_id: String,
    /// The Amazon ECS cluster backing the compute environment that took the job — the cluster
    /// this service holds for that environment ([`super::ecs_cluster`]).
    pub cluster: String,
    /// The Region whose Amazon ECS holds that cluster: the Region of the job itself.
    pub region: String,
    /// The task definition family the registration uses — the name of the job definition
    /// revision the job resolved to, which is how AWS Batch groups the task definitions of one
    /// job definition.
    pub family: String,
    /// The container the job actually runs ([`effective_container`]).
    pub container: ContainerDetail,
    /// The subnets the task's elastic network interface may be created in
    /// (`ComputeResource$subnets`).
    pub subnets: Vec<String>,
    /// The security groups that interface is governed by (`ComputeResource$securityGroupIds`).
    pub security_groups: Vec<String>,
    /// The Fargate platform version the job's container asked for, if any
    /// (`FargatePlatformConfiguration$platformVersion`).
    pub platform_version: Option<String>,
    /// The tags to put on the task, which is what `propagateTags` asks for: "specifies whether
    /// to propagate the tags from the job or job definition to the corresponding Amazon ECS
    /// task". Empty for a job that asked for no propagation.
    pub tags: Vec<(String, String)>,
}

/// ONE step this service owes Amazon ECS for one job.
#[derive(Debug, Clone)]
enum Step {
    /// Register the task definition the job's container runs as and run it in the cluster.
    Run(Box<TaskPlacement>),
    /// Read the task's own progress.
    Poll {
        job_id: String,
        region: String,
        cluster: String,
        task_arn: String,
    },
    /// End the task of a job that has ended.
    Stop {
        job_id: String,
        region: String,
        cluster: String,
        task_arn: String,
    },
}

impl Step {
    /// The job this step belongs to.
    fn job_id(&self) -> &str {
        match self {
            Self::Run(placement) => &placement.job_id,
            Self::Poll { job_id, .. } | Self::Stop { job_id, .. } => job_id,
        }
    }
}

/// What ONE step answered.
#[derive(Debug, Clone)]
enum Outcome {
    /// Amazon ECS holds the task, under this ARN.
    Accepted { task_arn: String },
    /// Amazon ECS REFUSED to run it, saying this.
    Refused { reason: String },
    /// The call reached nothing at all, so no container of this job was ever started.
    Unreached,
    /// The READ of a task this service holds could not be answered, so it says nothing about
    /// the run — told apart from [`Outcome::Unreached`], which is a RUN nothing took.
    Unread,
    /// The task's own progress, as Amazon ECS reports it.
    Reported(TaskReport),
    /// The end of the task was asked for.
    StopAsked,
}

/// What ONE read of a job's task found: the task's status and instants, and what its container
/// has produced.
///
/// Every member is read off `DescribeTasks` — nothing is derived here — so the job and the task
/// cannot disagree about the run.
#[derive(Debug, Clone, Default)]
pub struct TaskReport {
    /// `Task$lastStatus`: "the last known status for the task".
    pub last_status: String,
    /// `Task$startedAt`: "the Unix timestamp for the time when the task started", in epoch
    /// milliseconds.
    pub started_at: Option<i64>,
    /// `Task$stoppedAt`: "the Unix timestamp for the time when the task was stopped".
    pub stopped_at: Option<i64>,
    /// `Task$stoppedReason`: "the reason that the task was stopped".
    pub stopped_reason: Option<String>,
    /// `Task$stopCode`: "the stop code indicating why a task was stopped", which is what
    /// tells a task whose container EXITED from one that never started at all
    /// (`smithy_query(query="shape", shape="TaskStopCode")`).
    pub stop_code: Option<String>,
    /// `Container$exitCode`: "the exit code returned from the container", and `None` for a
    /// container that produced no exit status.
    pub exit_code: Option<i32>,
    /// `Container$reason`: "a short (255 max characters) human-readable string to provide
    /// additional details about a running or stopped container".
    pub container_reason: Option<String>,
    /// `Container$networkInterfaces`: "the network interfaces associated with the container",
    /// which an `awsvpc` task's container is reached on.
    pub network_interfaces: Vec<NetworkInterface>,
}

impl TaskReport {
    /// Whether the task has STOPPED, the status at which everything about the run is final.
    fn stopped(&self) -> bool {
        self.last_status == TASK_STATUS_STOPPED
    }
}

/// `Task$lastStatus` of a task that has stopped — the terminal status of the task lifecycle
/// (`smithy_query(query="doc", shape="Task", member="lastStatus")` points at that lifecycle).
const TASK_STATUS_STOPPED: &str = "STOPPED";

/// The `TaskStopCode` of a task that stopped because its essential container exited: "the task
/// stopped because an essential container exited". It is the stop of a task that RAN, so what
/// the job reports for it is what that container exited with — and when no exit status was
/// produced at all, that nothing started it.
const STOP_CODE_ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The `name` of the one container of the task a container job runs as, which is also the
/// middle segment of the log stream its output goes to ([`job_log_stream_name`]).
const TASK_CONTAINER_NAME: &str = LOG_STREAM_CONTAINER_NAME;

/// How many tags a propagating job may put on the Amazon ECS task it runs as.
///
/// `SubmitJob$propagateTags`: "if the total number of combined tags from the job and job
/// definition is over 50, the job is moved to the FAILED state" — the ceiling is on the
/// COMBINED set, which is the set the propagation would carry
/// ([`ComAmazonawsBatchAwsbatchv20160810State::propagated_task_tags`]), and it is the
/// propagation's own: a job that propagates nothing is held to no such number.
pub const MAX_PROPAGATED_TASK_TAGS: usize = 50;

/// `statusReason` of a job whose propagation would carry more than
/// [`MAX_PROPAGATED_TASK_TAGS`] tags.
///
/// The condition the documented sentence names, in that sentence's own words and with the
/// number read off the ceiling itself, because the member states the rule and the state the
/// job is moved to without publishing a second wording for it.
pub fn propagated_tags_over_limit_reason() -> String {
    format!(
        "The total number of combined tags from the job and job definition is over \
         {MAX_PROPAGATED_TASK_TAGS}"
    )
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// Where the job `id` would RUN as an Amazon ECS task, or `None` when the capacity its
    /// queue offers cannot take one.
    ///
    /// Every value is read off the compute environment the scheduler would place the job on
    /// ([`Self::placement_environment`]) and off the job's own effective container, so the task
    /// runs in the cluster that environment holds, on the subnets and security groups that
    /// environment publishes. `None` for an environment whose capacity is Amazon EC2 instances
    /// (no host of the account is registered into its cluster, so nothing there can run the
    /// task), one this service does not orchestrate with Amazon ECS, one whose cluster nothing
    /// holds yet, and a job whose container states no Fargate reservation to run under — see
    /// this module's header.
    pub(super) fn task_placement(&self, id: &str) -> Option<TaskPlacement> {
        let job = self.jobs.get(id)?;
        let container = job.container.clone()?;
        let environment = self.placement_environment(id)?;
        if !environment.orchestrated_with_ecs() {
            return None;
        }
        let resources = environment.compute_resources.as_ref()?;
        if !is_fargate_compute_type(&resources.r#type) {
            return None;
        }
        let cluster = environment.ecs_cluster_name.clone()?;
        // The reservation a Fargate task runs under is the TASK's own `cpu` and `memory`, which
        // a Fargate job definition states as its `resourceRequirements`
        // ([`fargate_reservation_refusal`] is what holds one to the supported pairs); a
        // container stating neither names no size for the task, and nothing is invented for it.
        fargate_task_size(&container)?;
        Some(TaskPlacement {
            job_id: job.job_id.clone(),
            cluster,
            region: arn_region(&job.job_arn).unwrap_or(DEFAULT_REGION).to_string(),
            family: job_definition_reference_name(&job.job_definition_arn).to_string(),
            subnets: resources.subnets.clone().unwrap_or_default(),
            security_groups: resources.security_group_ids.clone().unwrap_or_default(),
            platform_version: container
                .fargate_platform_configuration
                .as_ref()
                .and_then(|configuration| configuration.platform_version.clone()),
            // What a job propagating its tags puts on the task is BOTH sets merged
            // ([`Self::propagated_task_tags`]). A job that asked for no propagation propagates
            // none.
            tags: if job.propagate_tags.unwrap_or(false) {
                self.propagated_task_tags(job)
            } else {
                Vec::new()
            },
            container,
        })
    }

    /// The tags a job propagating them puts on the Amazon ECS task it runs as: the tags of the
    /// job definition revision it runs under, with the job's OWN tags over them.
    ///
    /// `JobDetail$propagateTags` is "whether to propagate the tags from the job OR JOB
    /// DEFINITION to the corresponding Amazon ECS task", and it names the tie-break for the two
    /// sets it merges: "for tags with the same name, job tags are given priority over job
    /// definitions tags". So both sets travel, the job's value winning on a key they share —
    /// and the tags a reader sees on each resource are unchanged by that, since the merge is
    /// made for the TASK and written nowhere else.
    ///
    /// Held in a [`BTreeMap`], so the merge decides each key once and the list the dispatch
    /// carries is in key order whatever order either set was written in.
    fn propagated_task_tags(&self, job: &Job) -> Vec<(String, String)> {
        let mut merged: BTreeMap<String, String> = self
            .job_definition(&job.job_definition_arn)
            .and_then(|definition| definition.tags.clone())
            .unwrap_or_default()
            .into_iter()
            .collect();
        merged.extend(job.tags.clone().unwrap_or_default());
        merged.into_iter().collect()
    }

    /// Whether the propagation `id` asked for would put MORE tags on the task it runs as than
    /// the propagation allows ([`MAX_PROPAGATED_TASK_TAGS`]).
    ///
    /// Read off the same merged set the propagation would carry
    /// ([`Self::propagated_task_tags`]), so what decides the job's end is exactly what would
    /// have travelled — one entry per key, the tie-break already applied. A job that asked for
    /// no propagation puts no tags on any task, so no ceiling applies to it.
    ///
    /// Costs the one merge of the two tag sets, each of which the model's own `TagrisTagsMap`
    /// bounds.
    pub(super) fn propagation_exceeds_task_tag_limit(&self, id: &str) -> bool {
        let Some(job) = self.jobs.get(id) else {
            return false;
        };
        if !job.propagate_tags.unwrap_or(false) {
            return false;
        }
        self.propagated_task_tags(job).len() > MAX_PROPAGATED_TASK_TAGS
    }

    /// Owe Amazon ECS the task `id`'s container runs as, and take the job out of the clock's
    /// hands until that task exists.
    ///
    /// Called from the ONE place a placement falls due, so the dispatch is owed once per entry
    /// into it. The job stays in `RUNNABLE` — where a job waits to be placed — because nothing
    /// has placed it yet: the drain that follows this materialisation is what runs the task,
    /// and the attempt a reader sees is the one that task made.
    pub(super) fn arm_task_dispatch(&mut self, id: &str) {
        self.set_job_due(id, None);
        if let Some(job) = self.jobs.get_mut(id) {
            job.task_dispatch = JobTaskDispatch::Due;
            job.task_attempted_at = None;
        }
    }

    /// Whether this store owes Amazon ECS a task RUN right now, which is what keeps the clock
    /// pump awake until every placed job's task exists.
    ///
    /// Only a run that is OWED is "now": a dispatch already in flight falls due at the end of
    /// its retry window and the READS that follow a created task fall due on the job's own
    /// clock ([`JOB_TASK_POLL_MILLIS`]), so both keep the pump awake through the due index like
    /// every other transition this service owes rather than spinning it.
    ///
    /// Costs one walk of the jobs map, which [`limits::MAX_JOBS`] bounds — the same walk
    /// [`Self::rearm_resting_jobs`] makes.
    pub(super) fn has_pending_job_tasks(&self) -> bool {
        self.jobs
            .values()
            .any(|job| job.task_dispatch == JobTaskDispatch::Due)
    }

    /// The steps this service owes Amazon ECS at `now`, with each job marked as having its step
    /// taken, so two concurrent callers never run one job's container twice.
    ///
    /// Walked in job-id order and capped at [`MAX_TASK_STEPS_PER_CALL`], so which steps a drain
    /// takes depends on the store and not on how a map happened to be iterated.
    fn take_task_steps(&mut self, now: i64) -> Vec<Step> {
        let mut ids: Vec<String> = self
            .jobs
            .values()
            .filter(|job| job.task_dispatch != JobTaskDispatch::Waiting)
            .map(|job| job.job_id.clone())
            .collect();
        ids.sort();
        let mut steps: Vec<Step> = Vec::new();
        for id in ids {
            if steps.len() >= MAX_TASK_STEPS_PER_CALL {
                break;
            }
            // Everything the decision reads is taken off the record HERE, so the step below is
            // chosen with no borrow of the store outstanding.
            let Some((dispatch, terminal, attempted_at, task_arn, cluster, region)) =
                self.jobs.get(&id).map(|job| {
                    (
                        job.task_dispatch,
                        is_terminal_status(&job.status),
                        job.task_attempted_at,
                        job.task_arn.clone(),
                        job.task_cluster.clone(),
                        arn_region(&job.job_arn).unwrap_or(DEFAULT_REGION).to_string(),
                    )
                })
            else {
                continue;
            };
            let in_flight_expired = attempted_at
                .is_none_or(|at| now.saturating_sub(at) >= TASK_DISPATCH_RETRY_AFTER_MILLIS);
            let step = match dispatch {
                // A job something ended before its container was ever run has none to run.
                JobTaskDispatch::Due | JobTaskDispatch::InFlight if terminal => {
                    self.give_up_on_a_task(&id, now);
                    continue;
                }
                JobTaskDispatch::Due | JobTaskDispatch::InFlight
                    if dispatch == JobTaskDispatch::Due || in_flight_expired =>
                {
                    match self.task_placement(&id) {
                        Some(placement) => Some(Step::Run(Box::new(placement))),
                        // The capacity that was going to take the task cannot any more — the
                        // environment behind the queue was changed or removed between the
                        // placement falling due and this drain. Nothing is owed to Amazon ECS
                        // for the job, so the dispatch is given up and the job is left to the
                        // placement every job on capacity that can take no task gets, which
                        // the clock makes at the instant below.
                        None => {
                            self.give_up_on_a_task(&id, now);
                            continue;
                        }
                    }
                }
                JobTaskDispatch::Running => match (task_arn, cluster) {
                    (Some(task_arn), Some(cluster)) if terminal => {
                        // The job ended while its task was still running — a cancel, a
                        // terminate, the deletion of its queue — so the task ends with it.
                        Some(Step::Stop {
                            job_id: id.clone(),
                            region,
                            cluster,
                            task_arn,
                        })
                    }
                    (Some(task_arn), Some(cluster)) => Some(Step::Poll {
                        job_id: id.clone(),
                        region,
                        cluster,
                        task_arn,
                    }),
                    // A job in this state carries the task the run was accepted as, so there
                    // is nothing to address: the dispatch is given up rather than left owing
                    // a step nothing can take.
                    _ => {
                        self.give_up_on_a_task(&id, now);
                        continue;
                    }
                },
                _ => None,
            };
            let Some(step) = step else {
                continue;
            };
            if let Some(job) = self.jobs.get_mut(&id) {
                match &step {
                    Step::Run(_) => {
                        job.task_dispatch = JobTaskDispatch::InFlight;
                        job.task_attempted_at = Some(now);
                    }
                    Step::Stop { .. } => job.task_dispatch = JobTaskDispatch::Stopped,
                    Step::Poll { .. } => {}
                }
            }
            if matches!(step, Step::Run(_)) {
                // A dispatch whose answer never comes is attempted again at the end of its
                // retry window, and that instant is what the clock waits for — so a lost
                // dispatch costs one wake-up then rather than keeping the pump awake until it.
                self.set_job_due(&id, Some(now + TASK_DISPATCH_RETRY_AFTER_MILLIS));
            }
            steps.push(step);
        }
        steps
    }

    /// Give up on the Amazon ECS task of `id`: nothing is owed to that service for the job any
    /// more, and the CLOCK is what decides what the job does next, at `now`.
    ///
    /// The ONE place a dispatch is abandoned, so no state this store can reach leaves a job
    /// owing a step nothing will take ([`Self::has_pending_job_tasks`] is what keeps the clock
    /// pump awake while one is owed, and this is what clears it): the job goes back on the due
    /// index, where [`ComAmazonawsBatchAwsbatchv20160810State::advance_job`] gives a job still
    /// waiting in `RUNNABLE` the placement every job on capacity that can take no task gets,
    /// and ends a job whose task is gone.
    fn give_up_on_a_task(&mut self, id: &str, now: i64) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.task_dispatch = JobTaskDispatch::Waiting;
            job.task_attempted_at = None;
            job.unanswered_task_reads = 0;
        }
        if self
            .jobs
            .get(id)
            .is_some_and(|job| !is_terminal_status(&job.status))
        {
            self.set_job_due(id, Some(now));
        }
    }

    /// Record what one step answered for `job_id`.
    fn record_task_outcome(
        &mut self,
        job_id: &str,
        placed: Option<&TaskPlacement>,
        outcome: Outcome,
        now: i64,
    ) {
        match outcome {
            Outcome::Accepted { task_arn } => {
                let Some(placement) = placed else {
                    return;
                };
                if let Some(job) = self.jobs.get_mut(job_id) {
                    job.task_dispatch = JobTaskDispatch::Running;
                    job.task_cluster = Some(placement.cluster.clone());
                    job.task_arn = Some(task_arn.clone());
                }
                // A job something ENDED while its task was being created is not placed on
                // that task: the end its caller wrote stands, and the task is stopped with it
                // ([`Self::take_task_steps`]'s `Stop` step, which the record above is what
                // lets it address).
                if self
                    .jobs
                    .get(job_id)
                    .is_none_or(|job| is_terminal_status(&job.status))
                {
                    return;
                }
                // The task EXISTS, so this is the placement: the attempt it started is
                // recorded carrying that task's ARN and the stream its container writes to,
                // and the job is reported in `STARTING`, where "the relevant container
                // initiation operations are underway".
                let task_id = ecs_task_id(&task_arn);
                self.record_placement(job_id, task_arn, &task_id, &placement.region);
                self.start_placed_job(job_id, now);
                self.set_job_due_bounded_by_attempt_deadline(job_id, now + JOB_TASK_POLL_MILLIS);
            }
            // Amazon ECS would not run the container at all: no task was created, so nothing
            // of this emulator started the job's container. The refusal is that service's
            // verdict on the request this service built for the capacity — a sentence AWS
            // Batch itself never puts on a job — so it is TRACED, and the job takes the
            // placement and the end a placed job whose container nothing ran takes.
            Outcome::Refused { reason } => {
                ::tracing::warn!(
                    job_id = %job_id,
                    reason = %reason,
                    "Amazon ECS would not run the task a placed Batch job's container was to \
                     run as, so nothing started it"
                );
                self.record_task_outcome(job_id, placed, Outcome::Unreached, now);
            }
            // Nothing served the call, so no TASK runs this job's container: the placement is
            // made by this service instead — the task it names for the attempt, the stream
            // that task's container writes to — and the program the container declares is
            // performed in this process, like that of every job on capacity that can take no
            // task ([`super::container_run`]).
            Outcome::Unreached => {
                if let Some(job) = self.jobs.get_mut(job_id) {
                    job.task_dispatch = JobTaskDispatch::Waiting;
                }
                if self
                    .jobs
                    .get(job_id)
                    .is_some_and(|job| job.status == JOB_RUNNABLE)
                    && self.place_job(job_id)
                {
                    self.start_placed_job(job_id, now);
                }
            }
            Outcome::Reported(report) => self.record_task_report(job_id, &report, now),
            // The read said nothing about the run, so the job keeps the status it has and the
            // read is owed again — up to [`MAX_UNANSWERED_TASK_READS`] of them, past which the
            // task is one this service can learn nothing more from: the job's work is over
            // with no outcome to report for it ([`container_lost_its_task_reason`]).
            Outcome::Unread => {
                let spent = self
                    .jobs
                    .get_mut(job_id)
                    .map(|job| {
                        job.unanswered_task_reads = job.unanswered_task_reads.saturating_add(1);
                        job.unanswered_task_reads
                    })
                    .unwrap_or_default();
                if self
                    .jobs
                    .get(job_id)
                    .is_none_or(|job| is_terminal_status(&job.status))
                {
                    self.give_up_on_a_task(job_id, now);
                } else if spent >= MAX_UNANSWERED_TASK_READS {
                    self.give_up_on_a_task(job_id, now);
                    let reason = container_lost_its_task_reason();
                    // The attempt produced no outcome at all, which is a failed attempt: a job
                    // with attempts left gets another one, on a task of its own
                    // ([`super::job_retries`]).
                    self.end_job_attempt(job_id, JOB_FAILED, Some(&reason), now);
                } else {
                    self.set_job_due_bounded_by_attempt_deadline(job_id, now + JOB_TASK_POLL_MILLIS);
                }
            }
            Outcome::StopAsked => {
                if let Some(job) = self.jobs.get_mut(job_id) {
                    job.task_dispatch = JobTaskDispatch::Waiting;
                }
            }
        }
    }

    /// Record what ONE read of a job's task found: the job's status, instants and attempt are
    /// the task's, read off [`TaskReport`].
    ///
    /// A job something else has already ended keeps the end that put it there; everything else
    /// follows the task — `RUNNING` while its container is running, and a terminal status when
    /// the task has stopped: `SUCCEEDED` for a container that exited 0 ("the exit code returned
    /// from the container"; a job "reaches SUCCEEDED when its essential container exits 0"),
    /// `FAILED` for one that exited with anything else, and `FAILED` saying nothing started it
    /// for a container that produced no exit status at all.
    ///
    /// A job whose task is still RUNNING settles too once that task reports a container
    /// DETAIL with no exit status beside it: such a container has produced the only account of
    /// its run there will be, so the job's work is over and it reports `FAILED` carrying that
    /// detail — rather than following, for ever, a task whose container owes it no status.
    fn record_task_report(&mut self, job_id: &str, report: &TaskReport, now: i64) {
        if self
            .jobs
            .get(job_id)
            .is_none_or(|job| is_terminal_status(&job.status))
        {
            return;
        }
        // The task answered, so the reads that said nothing before it are behind this job.
        if let Some(job) = self.jobs.get_mut(job_id) {
            job.unanswered_task_reads = 0;
        }
        // The task started its containers, so the job RAN: `JobDetail$startedAt` is "when the
        // job transitioned from the `STARTING` state to the `RUNNING` state", which is the
        // instant the task reports, and the job is reported there whether or not this same read
        // also finds the task stopped — so a client watching the job through EventBridge sees
        // every status it took. Its attempt started then too.
        if let Some(started_at) = report.started_at {
            if let Some(job) = self.jobs.get_mut(job_id) {
                job.started_at = Some(started_at);
                if let Some(attempt) = job.attempts.last_mut() {
                    attempt.started_at = Some(started_at);
                }
            }
            if self
                .jobs
                .get(job_id)
                .is_some_and(|job| job.status != JOB_RUNNING)
            {
                self.set_job_status(job_id, JOB_RUNNING, started_at);
            }
        }
        if !report.stopped() {
            // The task is still running, but its container may already have produced
            // everything it ever will. `Container$reason` is "a short human-readable string to
            // provide additional details about a RUNNING or stopped container", and
            // `AttemptContainerDetail$reason` is where AWS Batch reports that same detail for
            // the attempt — so a container the task reports a detail for while reporting NO
            // exit status has produced the only account of its run there will be: no exit
            // status is owed by anything still running it. The job's work is over, so it is
            // reported FAILED carrying that detail, with no exit code invented for a container
            // that produced none — and the task it was placed as is stopped with the job, the
            // way the task of any job that ends while running one is
            // ([`Self::take_task_steps`]'s `Stop` step).
            if report.exit_code.is_none() {
                if let Some(detail) = report
                    .container_reason
                    .as_deref()
                    .filter(|detail| !detail.is_empty())
                    .map(str::to_string)
                {
                    // What the run DID produce is on the record before the end is written: the
                    // interface the task reports its container was reached on is the attempt's,
                    // exactly as it is for a container that exited ([`Self::record_task_report`]
                    // below). No exit code is written: the container produced none.
                    if let Some(job) = self.jobs.get_mut(job_id) {
                        if let Some(attempt) = job.attempts.last_mut() {
                            attempt.network_interfaces = report.network_interfaces.clone();
                        }
                        if let Some(container) = job.container.as_mut() {
                            container.network_interfaces =
                                Some(report.network_interfaces.clone());
                        }
                    }
                    // No retry strategy is consulted here, and this is the one failure of an
                    // attempt where none is: the task this attempt was placed as is STILL
                    // RUNNING, so the job's own end is what stops it
                    // ([`Self::take_task_steps`]'s `Stop` step, which addresses the task of a
                    // job that has gone terminal). A retry would leave that task running
                    // beside the one the next attempt is placed as, which is one container of
                    // one attempt running twice.
                    self.finish_job(job_id, JOB_FAILED, Some(&detail), now);
                    return;
                }
            }
            // The task is still going, so the next read is owed — or the job's own attempt
            // deadline is, when that falls first ([`super::attempt_timeout`]).
            self.set_job_due_bounded_by_attempt_deadline(job_id, now + JOB_TASK_POLL_MILLIS);
            return;
        }
        // The task has stopped: what its container produced is final, so it is recorded on the
        // attempt the placement started before the job's own end is written.
        let ended_at = report.stopped_at.unwrap_or(now);
        if let Some(job) = self.jobs.get_mut(job_id) {
            job.task_dispatch = JobTaskDispatch::Waiting;
            if let Some(attempt) = job.attempts.last_mut() {
                attempt.exit_code = report.exit_code;
                attempt.network_interfaces = report.network_interfaces.clone();
            }
            if let Some(container) = job.container.as_mut() {
                container.exit_code = report.exit_code;
                container.network_interfaces = Some(report.network_interfaces.clone());
            }
        }
        match report.exit_code {
            Some(0) => self.finish_job(
                job_id,
                JOB_SUCCEEDED,
                report.stopped_reason.as_deref(),
                ended_at,
            ),
            // The attempt FAILED, and whether that ends the job is its retry strategy's to
            // decide ([`super::job_retries`]): a job with attempts left is moved back to
            // `RUNNABLE`, where the next placement runs its container as a task of its own.
            Some(_) => {
                self.end_job_attempt(
                    job_id,
                    JOB_FAILED,
                    report
                        .container_reason
                        .as_deref()
                        .or(report.stopped_reason.as_deref()),
                    ended_at,
                );
            }
            // The task stopped without its container ever producing an exit status. What that
            // says about the job is the task's own stop CODE: a task that never started — one
            // whose image could not be fetched, whose execution resources could not be
            // retrieved — reports why it did not, and that is the reason the job carries, the
            // same class of reason a failed fetch gives it ([`image_pull`]). A task that
            // stopped because its essential container exited, with no status from that
            // container, performed nothing: the program the container declares was never
            // started, which is what the job reports — in the words this service words that
            // with wherever it happens ([`container_run`]) — rather than an outcome for work
            // nothing ran.
            None => {
                let exited = report
                    .stop_code
                    .as_deref()
                    .is_none_or(|code| code == STOP_CODE_ESSENTIAL_CONTAINER_EXITED);
                let declared = self.jobs.get(job_id).and_then(|job| job.container.clone());
                let reason = match report.stopped_reason.as_deref() {
                    Some(reason) if !exited => reason.to_string(),
                    _ => nothing_performed_reason(declared.as_ref()),
                };
                self.end_job_attempt(job_id, JOB_FAILED, Some(&reason), ended_at);
            }
        }
    }
}

/// How this emulator states that the run of a job's container produced NOTHING: the program
/// the container declares was never started, so no status is claimed for it.
///
/// One vocabulary for that end wherever it is reached: the words are
/// [`container_run`]'s — the ones a job whose declared program this emulator performs none of
/// carries — so a task whose container produced no exit status reads the same as a program
/// nothing performed in this process, because the two are the same fact about the job.
fn nothing_performed_reason(container: Option<&ContainerDetail>) -> String {
    let declared = container.and_then(|container| container.command.as_deref());
    match container_program(declared).first() {
        Some(program) => program_not_performed(program),
        None => no_declared_program(container.and_then(|container| container.image.as_deref())),
    }
}

/// How this emulator states that the Amazon ECS task a job's container was RUNNING as can no
/// longer be read, so nothing more can be learned about the run ([`MAX_UNANSWERED_TASK_READS`]
/// reads of it said nothing at all).
///
/// Not an AWS sentence: AWS Batch never loses the task of a running job. It claims nothing
/// about an exit status, because the container produced none this service ever saw.
pub fn container_lost_its_task_reason() -> String {
    "The Amazon ECS task this job's container was running as can no longer be read, so the \
     container produced no outcome for the job to report."
        .to_string()
}

/// One instant Amazon ECS reported, in the epoch milliseconds every instant of this service
/// is held as — and `None` for a value outside what that fits, which is no instant this
/// emulator could have produced.
fn epoch_millis(at: &::aws_smithy_types::DateTime) -> Option<i64> {
    at.to_millis().ok()
}

/// The 32-character task id of the Amazon ECS task `arn`, which is the last segment of
/// `task/<cluster>/<task id>` and the segment the task's log stream is named with
/// ([`job_log_stream_name`]).
fn ecs_task_id(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
}

/// The TASK `cpu` and `memory` a Fargate job's container runs under, read off its
/// `resourceRequirements`, or `None` when it states neither.
///
/// `ResourceRequirement$type` `VCPU` is "the number of vCPUs reserved for the container" and
/// `MEMORY` "the memory hard limit (in MiB)", which for a job on Fargate resources are the
/// task's own size: "for jobs that run on Fargate resources, then `value` must match one of the
/// supported values" — the vCPU/memory pairs [`FARGATE_VCPU_MEMORY`] enumerates. Amazon ECS
/// takes the same two as `cpu` in CPU UNITS ("1024 CPU units is the equivalent of 1 vCPU") and
/// `memory` in MiB, so the vCPU count is converted and the memory travels as it stands.
fn fargate_task_size(container: &ContainerDetail) -> Option<(String, String)> {
    let requirements = container.resource_requirements.as_deref().unwrap_or_default();
    let vcpus: f64 = stated_resource_value(requirements, RESOURCE_TYPE_VCPU)?
        .trim()
        .parse()
        .ok()?;
    let memory: i64 = stated_resource_value(requirements, RESOURCE_TYPE_MEMORY)?
        .trim()
        .parse()
        .ok()?;
    if vcpus <= 0.0 || memory <= 0 {
        return None;
    }
    // 1024 CPU units per vCPU (`RegisterTaskDefinition$cpu`), rounded to the nearest unit so a
    // fractional vCPU the job asked for is the unit count Amazon ECS publishes for it.
    let units = (vcpus * 1024.0).round() as i64;
    Some((units.to_string(), memory.to_string()))
}

/// Run every task step this store owes Amazon ECS, and record what each one answered.
///
/// Called by the drain every operation of this service's job plane makes, and by the clock
/// pump, with this store's guard dropped: the steps are collected under one guard, performed
/// with none held, and recorded under another.
pub(super) async fn settle_job_tasks(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    state: &::engine::ServiceState<ComAmazonawsBatchAwsbatchv20160810State>,
) {
    let now = now_millis();
    // The guard is taken and dropped inside this statement: the steps are moved out here so no
    // step is taken twice, and no lock is held across an await.
    let steps = state.lock().take_task_steps(now);
    if steps.is_empty() {
        return;
    }
    let mut answered: Vec<(String, Option<TaskPlacement>, Outcome)> =
        Vec::with_capacity(steps.len());
    for step in steps {
        let outcome = perform(callback, &step).await;
        let placed = match &step {
            Step::Run(placement) => Some((**placement).clone()),
            _ => None,
        };
        answered.push((step.job_id().to_string(), placed, outcome));
    }
    let mut store = state.lock();
    let settled_at = now_millis();
    for (job_id, placed, outcome) in answered {
        store.record_task_outcome(&job_id, placed.as_ref(), outcome, settled_at);
    }
}

/// Perform one step against Amazon ECS.
async fn perform(callback: &smithy_server_runtime::EmbeddedEmulator, step: &Step) -> Outcome {
    match step {
        Step::Run(placement) => run_task(callback, placement).await,
        Step::Poll {
            region,
            cluster,
            task_arn,
            ..
        } => read_task(callback, region, cluster, task_arn).await,
        Step::Stop {
            region,
            cluster,
            task_arn,
            ..
        } => {
            // Whatever the stop answers, it has been ASKED: the job is already in its terminal
            // status, which its caller put it in, so nothing the answer carries changes what a
            // reader sees.
            let _ = ecs_client(callback, region)
                .stop_task()
                .cluster(cluster)
                .task(task_arn)
                .reason(TASK_STOPPED_WITH_JOB_REASON)
                .send()
                .await;
            Outcome::StopAsked
        }
    }
}

/// Register the task definition one job's container runs as and RUN it in the cluster behind
/// the job's compute environment.
///
/// The registration carries what DECIDES the run and nothing else: the image the container runs
/// and the role the pull is made as, the program (`entryPoint`/`command`), the environment and
/// the `secrets` the agent fetches for it, the log configuration its output is delivered
/// through, the role the container's own calls are made as, and the task size and platform it
/// runs under. A member that configures a host this emulator has none of — a volume, a mount
/// point, a ulimit, a privilege — is left out rather than registered against a task where
/// nothing applies it.
async fn run_task(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    placement: &TaskPlacement,
) -> Outcome {
    let client = ecs_client(callback, &placement.region);
    let Some((cpu, memory)) = fargate_task_size(&placement.container) else {
        return Outcome::Unreached;
    };
    let container = &placement.container;
    let mut definition = ecs::ContainerDefinition::builder()
        .name(TASK_CONTAINER_NAME)
        .essential(true)
        .set_image(container.image.clone())
        .set_command(container.command.clone())
        .set_user(container.user.clone());
    for pair in container.environment.iter().flatten() {
        definition = definition.environment(
            ecs::KeyValuePair::builder()
                .set_name(pair.name.clone())
                .set_value(pair.value.clone())
                .build(),
        );
    }
    for secret in container.secrets.iter().flatten() {
        if let Ok(built) = ecs::Secret::builder()
            .name(&secret.name)
            .value_from(&secret.value_from)
            .build()
        {
            definition = definition.secrets(built);
        }
    }
    // WHERE the container's output goes is the driver the job's own `logConfiguration` selects,
    // read through the one place this service resolves that delivery ([`job_log_target`]), so
    // the stream Amazon ECS opens for the task is the stream this service reports for the
    // attempt.
    if let Some(target) = job_log_target(container.log_configuration.as_ref(), "", &placement.region)
    {
        let prefix = job_log_stream_prefix(container.log_configuration.as_ref(), &placement.family);
        let mut options = ecs::LogConfiguration::builder()
            .log_driver(ecs::LogDriver::Awslogs)
            .options("awslogs-group", &target.log_group)
            .options("awslogs-region", &target.region)
            .options("awslogs-stream-prefix", &prefix);
        if target.service_holds_group {
            // The group AWS Batch holds for its jobs' output is THIS SERVICE's to create
            // ([`job_logs`]), so it is created here — before the task whose agent delivers
            // into it exists — rather than left to that agent, which would make the job's
            // execution role need a permission to create a group this service already holds.
            create_job_log_group(callback, &target.region, &target.log_group).await;
        }
        if target.driver_creates_group {
            // A group the job's own configuration asked the DRIVER to create is the driver's
            // creation, so the task is given that same instruction and the launch decides it.
            options = options.options("awslogs-create-group", "true");
        }
        if let Ok(configuration) = options.build() {
            definition = definition.log_configuration(configuration);
        }
    }
    let mut registration = client
        .register_task_definition()
        .family(&placement.family)
        .network_mode(ecs::NetworkMode::Awsvpc)
        .requires_compatibilities(ecs::Compatibility::Fargate)
        .cpu(&cpu)
        .memory(&memory)
        .set_execution_role_arn(container.execution_role_arn.clone())
        .set_task_role_arn(container.job_role_arn.clone())
        .container_definitions(definition.build());
    if let Some(platform) = container.runtime_platform.as_ref() {
        registration = registration.runtime_platform(
            ecs::RuntimePlatform::builder()
                .set_operating_system_family(
                    platform
                        .operating_system_family
                        .as_deref()
                        .map(ecs::OsFamily::from),
                )
                .set_cpu_architecture(
                    platform
                        .cpu_architecture
                        .as_deref()
                        .map(ecs::CpuArchitecture::from),
                )
                .build(),
        );
    }
    let task_definition_arn = match registration.send().await {
        Ok(answered) => answered
            .task_definition()
            .and_then(|definition| definition.task_definition_arn())
            .map(str::to_string),
        Err(error) => return refusal_outcome(&error, "register the task definition"),
    };
    let Some(task_definition_arn) = task_definition_arn else {
        return Outcome::Unreached;
    };
    let Ok(awsvpc) = ecs::AwsVpcConfiguration::builder()
        .set_subnets(Some(placement.subnets.clone()))
        .set_security_groups(Some(placement.security_groups.clone()))
        .set_assign_public_ip(
            container
                .network_configuration
                .as_ref()
                .and_then(|configuration| configuration.assign_public_ip.as_deref())
                .map(ecs::AssignPublicIp::from),
        )
        .build()
    else {
        return Outcome::Unreached;
    };
    let mut run = client
        .run_task()
        .cluster(&placement.cluster)
        .task_definition(&task_definition_arn)
        .launch_type(ecs::LaunchType::Fargate)
        .count(1)
        .set_platform_version(placement.platform_version.clone())
        .network_configuration(
            ecs::NetworkConfiguration::builder()
                .awsvpc_configuration(awsvpc)
                .build(),
        );
    for (key, value) in &placement.tags {
        run = run.tags(ecs::Tag::builder().key(key).value(value).build());
    }
    match run.send().await {
        Ok(answered) => match answered
            .tasks()
            .first()
            .and_then(|task| task.task_arn())
            .map(str::to_string)
        {
            Some(task_arn) => Outcome::Accepted { task_arn },
            // The call was served but placed nothing: `RunTask` reports why in its own
            // `failures` list ("any failures associated with the call"), which is what the
            // refusal of the run carries.
            None => Outcome::Refused {
                reason: run_failure_reason(answered.failures()),
            },
        },
        Err(error) => refusal_outcome(&error, "run the task"),
    }
}

/// Read one job's task, and answer what Amazon ECS reports for it.
///
/// A read that service could not answer — a refusal, a `tasks` list the task is not in — says
/// nothing about the run ([`Outcome::Unread`]): the job keeps the status it already had and the
/// read is made again, up to [`MAX_UNANSWERED_TASK_READS`] times.
async fn read_task(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    cluster: &str,
    task_arn: &str,
) -> Outcome {
    let answered = ecs_client(callback, region)
        .describe_tasks()
        .cluster(cluster)
        .tasks(task_arn)
        .send()
        .await;
    let Ok(answered) = answered else {
        return Outcome::Unread;
    };
    let Some(task) = answered.tasks().first() else {
        return Outcome::Unread;
    };
    let container = task.containers().first();
    Outcome::Reported(TaskReport {
        last_status: task.last_status().unwrap_or_default().to_string(),
        started_at: task.started_at().and_then(epoch_millis),
        stopped_at: task.stopped_at().and_then(epoch_millis),
        stopped_reason: task.stopped_reason().map(str::to_string),
        stop_code: task.stop_code().map(|code| code.as_str().to_string()),
        exit_code: container.and_then(::aws_sdk_ecs::types::Container::exit_code),
        container_reason: container
            .and_then(::aws_sdk_ecs::types::Container::reason)
            .map(str::to_string),
        network_interfaces: container
            .map(::aws_sdk_ecs::types::Container::network_interfaces)
            .unwrap_or_default()
            .iter()
            .map(|interface| NetworkInterface {
                attachment_id: interface.attachment_id().map(str::to_string),
                private_ipv4_address: interface.private_ipv4_address().map(str::to_string),
                ipv6_address: interface.ipv6_address().map(str::to_string),
            })
            .collect(),
    })
}

/// What `failures` says about a call that placed no task: the first failure's own reason and
/// detail, as Amazon ECS worded them — TRACED by the recorder of the outcome rather than put
/// on the job, because AWS Batch does not word a job's `statusReason` that way
/// ([`ComAmazonawsBatchAwsbatchv20160810State::record_task_outcome`]).
fn run_failure_reason(failures: &[ecs::Failure]) -> String {
    failures
        .first()
        .map(|failure| {
            let reason = failure.reason().unwrap_or_default();
            match failure.detail() {
                Some(detail) if !detail.is_empty() => format!("{reason}: {detail}"),
                _ => reason.to_string(),
            }
        })
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| String::from("the run placed no task and reported no failure"))
}

/// What one failed call to Amazon ECS says about the job: the REFUSAL that service made, or
/// nothing at all when the call reached no service.
///
/// A refusal is Amazon ECS's own verdict on the request this service built for the capacity;
/// what the job then reports is decided where the outcome is recorded
/// ([`ComAmazonawsBatchAwsbatchv20160810State::record_task_outcome`], which traces that verdict
/// rather than putting a sentence AWS Batch never words on the job). A call that reached
/// nothing — an emulator with no Amazon ECS mounted, a transport that answered no service —
/// says nothing about the container either, so the job takes the end a placed job takes when
/// nothing ran it.
fn refusal_outcome<E, R>(error: &::aws_sdk_ecs::error::SdkError<E, R>, attempted: &str) -> Outcome
where
    E: ::std::error::Error + ::aws_smithy_types::error::metadata::ProvideErrorMetadata,
{
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    let unserved = |code: &str| {
        code == ::service_common::in_process_refusal::UNKNOWN_SERVICE
            || code == ::service_common::in_process_refusal::AMBIGUOUS_SERVICE
    };
    match error.code() {
        Some(code) if !unserved(code) => {
            let reason = match error.message() {
                Some(message) if !message.is_empty() => message.to_string(),
                _ => code.to_string(),
            };
            Outcome::Refused { reason }
        }
        // The front door's own refusal, or a transport that answered no service at all: the
        // call reached nothing, so nothing is known about the job's container.
        _ => {
            ::tracing::warn!(
                attempted = %attempted,
                "Amazon ECS could not be reached to run a placed Batch job's container, so \
                 nothing started it"
            );
            Outcome::Unreached
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job of this store placed as the task `task_arn`, in `status`, with the dispatch state
    /// `dispatch` — the handful of members these rules are about, and the defaults for the rest
    /// ([`Job`]).
    fn placed(status: &str, dispatch: JobTaskDispatch) -> Job {
        Job {
            job_id: "job-1".to_string(),
            job_arn: ::engine::emulator_arn(ARN_SERVICE, DEFAULT_REGION, "job/job-1"),
            job_name: "a-job".to_string(),
            job_queue_arn: "q".to_string(),
            status: status.to_string(),
            task_dispatch: dispatch,
            task_arn: Some("arn:aws:ecs:us-east-1:000000000000:task/c/1".to_string()),
            task_cluster: Some("c".to_string()),
            ..Job::default()
        }
    }

    /// The store holding `job` and nothing else.
    fn holding(job: Job) -> ComAmazonawsBatchAwsbatchv20160810State {
        let mut store = ComAmazonawsBatchAwsbatchv20160810State::default();
        store.jobs.insert(job.job_id.clone(), job);
        store
    }

    /// RULE: a read of a job's task that says NOTHING leaves the job in the status it already
    /// had and is made again — up to [`MAX_UNANSWERED_TASK_READS`] of them, past which this
    /// service gives up on the task and the job ends with no outcome to report for its run.
    #[test]
    fn a_read_that_says_nothing_is_made_again_and_then_given_up_on() {
        // The rows differ along the status the job is in when its task stops answering: the
        // two non-terminal statuses a placed job of this service can be read in.
        for status in [JOB_STARTING, JOB_RUNNING] {
            let mut store = holding(placed(status, JobTaskDispatch::Running));
            for read in 1..MAX_UNANSWERED_TASK_READS {
                store.record_task_outcome("job-1", None, Outcome::Unread, 1_000 + i64::from(read));
                let job = store.jobs.get("job-1").expect("the job is held");
                assert_eq!(
                    job.status, status,
                    "case {status} read {read}: a read that said nothing must leave the status \
                     alone, got {job:?}"
                );
                assert_eq!(
                    job.task_dispatch,
                    JobTaskDispatch::Running,
                    "case {status} read {read}: the task is still this job's to read, got {job:?}"
                );
                assert_eq!(
                    job.next_phase_due_at,
                    Some(1_000 + i64::from(read) + JOB_TASK_POLL_MILLIS),
                    "case {status} read {read}: the next read must be owed, got {job:?}"
                );
                assert!(
                    !store.has_pending_job_tasks(),
                    "case {status} read {read}: a read owed on the clock must not keep the pump \
                     awake, got {:?}",
                    store.jobs.get("job-1")
                );
            }
            // The last one: the task is one nothing can learn anything more from, so the job
            // ends rather than resting in a status nothing will move again.
            store.record_task_outcome("job-1", None, Outcome::Unread, 9_000);
            let job = store.jobs.get("job-1").expect("the job is held");
            assert_eq!(
                job.status, JOB_FAILED,
                "case {status}: a job whose task stopped answering must end, got {job:?}"
            );
            assert_eq!(
                job.status_reason,
                Some(container_lost_its_task_reason()),
                "case {status}: the end must say the task it ran as can no longer be read, got \
                 {job:?}"
            );
            assert_eq!(
                job.next_phase_due_at, None,
                "case {status}: a terminal job owes the clock nothing, got {job:?}"
            );
            assert!(
                !store.has_pending_job_tasks(),
                "case {status}: nothing is owed Amazon ECS for an ended job, got {job:?}"
            );
        }
    }

    /// RULE: a dispatch this store can no longer place is GIVEN UP — it owes Amazon ECS no step
    /// nothing can take, and the job goes back on the clock, which places it the way it places
    /// every job on capacity that can take no task.
    #[test]
    fn a_dispatch_that_can_no_longer_be_placed_is_given_up_on() {
        // The rows differ along where the dispatch had got to when its placement stopped
        // resolving, and along what the job is: one still waiting to be placed, and one a
        // caller has already ended.
        let rows: &[(&str, &str, JobTaskDispatch)] = &[
            ("owed a run", JOB_RUNNABLE, JobTaskDispatch::Due),
            ("a run in flight", JOB_RUNNABLE, JobTaskDispatch::InFlight),
            ("owed a run, already ended", JOB_FAILED, JobTaskDispatch::Due),
            ("running with no task", JOB_RUNNING, JobTaskDispatch::Running),
        ];
        for (case, status, dispatch) in rows {
            let mut job = placed(status, *dispatch);
            // No container, so no capacity of this store can run a task for the job
            // ([`Self::task_placement`]); a `Running` row additionally holds no task to read.
            job.container = None;
            if *dispatch == JobTaskDispatch::Running {
                job.task_arn = None;
            }
            let mut store = holding(job);
            let steps = store.take_task_steps(5_000);
            assert!(
                steps.is_empty(),
                "case {case}: a job nothing can place owes no step, got {} of them",
                steps.len()
            );
            let job = store.jobs.get("job-1").expect("the job is held");
            assert_eq!(
                job.task_dispatch,
                JobTaskDispatch::Waiting,
                "case {case}: the dispatch must be given up, got {job:?}"
            );
            assert!(
                !store.has_pending_job_tasks(),
                "case {case}: a given-up dispatch must not keep the clock pump awake, got \
                 {job:?}"
            );
            let owed = if is_terminal_status(status) {
                None
            } else {
                Some(5_000)
            };
            assert_eq!(
                job.next_phase_due_at, owed,
                "case {case}: expected the clock to owe {owed:?}, got {job:?}"
            );
        }
    }

    /// RULE: a job whose task is still RUNNING settles as soon as that task reports a container
    /// DETAIL with no exit status beside it — such a container has produced the only account of
    /// its run there will be — and goes on being read while the task has an outcome still to
    /// produce.
    #[test]
    fn a_container_detail_with_no_exit_status_ends_the_job() {
        /// What one row's read of the task found, and what the job must report after it.
        struct Row {
            case: &'static str,
            /// `Container$exitCode`, which a container that produced no status has none of.
            exit_code: Option<i32>,
            /// `Container$reason`, the additional detail about the container.
            container_reason: Option<&'static str>,
            /// The `JobStatus` the job must be in after the read, and the `statusReason` it
            /// must carry — `None` where the job must still be running.
            settled: Option<(&'static str, &'static str)>,
        }
        // The rows differ along what the RUNNING task reports of its container: a detail with
        // no status (the run produced nothing), no detail at all (the run is still to answer),
        // and a detail beside a status the container really produced, which the stop of the
        // task is what finalises.
        let nothing_performed = "This emulator performs a container's declared program itself \
                                 and provides no program named \"simparity-worker\".";
        let rows: &[Row] = &[
            Row {
                case: "a detail with no exit status",
                exit_code: None,
                container_reason: Some(nothing_performed),
                settled: Some((JOB_FAILED, nothing_performed)),
            },
            Row {
                case: "no detail at all",
                exit_code: None,
                container_reason: None,
                settled: None,
            },
            Row {
                case: "an empty detail, which says nothing",
                exit_code: None,
                container_reason: Some(""),
                settled: None,
            },
            Row {
                case: "a detail beside an exit status the container produced",
                exit_code: Some(0),
                container_reason: Some("what the agent saw"),
                settled: None,
            },
        ];
        for row in rows {
            let case = row.case;
            let mut store = holding(placed(JOB_RUNNING, JobTaskDispatch::Running));
            let report = TaskReport {
                last_status: "RUNNING".to_string(),
                started_at: Some(1_000),
                exit_code: row.exit_code,
                container_reason: row.container_reason.map(str::to_string),
                ..TaskReport::default()
            };
            store.record_task_report("job-1", &report, 2_000);
            let job = store.jobs.get("job-1").expect("the job is held");
            match row.settled {
                Some((status, reason)) => {
                    assert_eq!(
                        job.status, status,
                        "case {case}: expected the job to settle {status}, got {job:?}"
                    );
                    assert_eq!(
                        job.status_reason.as_deref(),
                        Some(reason),
                        "case {case}: expected the task's own detail as the end, got {job:?}"
                    );
                    assert_eq!(
                        job.next_phase_due_at, None,
                        "case {case}: a settled job owes the clock nothing, got {job:?}"
                    );
                }
                None => {
                    assert_eq!(
                        job.status, JOB_RUNNING,
                        "case {case}: expected the job to go on running, got {job:?}"
                    );
                    assert_eq!(
                        job.next_phase_due_at,
                        Some(2_000 + JOB_TASK_POLL_MILLIS),
                        "case {case}: expected the next read of the task to be owed, got {job:?}"
                    );
                }
            }
        }
    }
}
