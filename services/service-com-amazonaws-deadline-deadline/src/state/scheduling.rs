// SPDX-License-Identifier: CC0-1.0

//! The scheduler: what a worker is given to do, and what reporting it back does.
//!
//! `UpdateWorkerSchedule` is the whole of the worker-facing protocol — the worker reports
//! what it did with the actions it holds and is given what to do next, in one call — so both
//! halves live here, on the store, because both read and write the same job, step, task and
//! session records.
//!
//! The rules this implements:
//!
//! * a worker is given work only from a queue its own fleet is associated with, through an
//!   association that is scheduling (`QueueFleetAssociationStatus=ACTIVE`): the association
//!   is what puts a queue's work on a fleet;
//! * among those queues' jobs, the one scheduled first is the highest priority, and the
//!   oldest of equal priority — `CreateJob` documents `priority` as "the highest priority
//!   (first scheduled) is 100. When two jobs have the same priority, the oldest job is
//!   scheduled first";
//! * a job is scheduled only while its own tasks are waiting to run (`taskRunStatus=READY`)
//!   and only onto as many workers at once as its `maxWorkerCount` allows;
//! * one worker runs one session at a time, and that session synchronises the job's
//!   attachments, enters the environments the job runs under — the queue's in priority
//!   order, then the job template's, then the step's — runs tasks of that one step, and
//!   exits those environments before it ends;
//! * a task a worker reports failed is run again while the job's `maxRetriesPerTask` allows
//!   it, and one the worker never ran, or was interrupted part way through, goes back to
//!   waiting without charging a retry;
//! * the job's own reported run status follows its tasks (see
//!   [`super::Job::settle_run_status_from_tasks`]).
//!
//! A job whose work this emulator did not fully read out of its template is placed on
//! nothing (see `super::job_template`): the alternative is handing a worker a task set that
//! is not the one the template describes.
//!
//! What one call of this costs, and what it can leave behind, is bounded by state the store
//! already bounds: a worker's report is resolved against its own sessions in one walk of
//! them ([`ComAmazonawsDeadlineDeadlineState::worker_action_locations`]), what an action
//! keeps of that report is the modelled-length text and manifest list the worker last sent
//! for it, a session holds at most [`super::MAX_SESSION_ACTIONS`] actions and a job at most
//! [`super::MAX_SESSIONS_PER_JOB`] sessions. Nothing here sleeps, spawns or waits.

use std::collections::HashMap;
use std::time::SystemTime;

use super::{
    ComAmazonawsDeadlineDeadlineState, Job, Session, SessionActionKind,
    COMPLETED_STATUS_CANCELED, COMPLETED_STATUS_FAILED, COMPLETED_STATUS_INTERRUPTED,
    COMPLETED_STATUS_NEVER_ATTEMPTED, COMPLETED_STATUS_SUCCEEDED, ENVIRONMENT_SCOPE_JOB,
    ENVIRONMENT_SCOPE_STEP, QUEUE_FLEET_ASSOCIATION_STATUS_ACTIVE,
    SESSION_ACTION_STATUS_RUNNING, SESSION_LIFECYCLE_ENDED, TASK_RUN_STATUS_ASSIGNED,
    TASK_RUN_STATUS_CANCELED, TASK_RUN_STATUS_FAILED, TASK_RUN_STATUS_READY,
    TASK_RUN_STATUS_RUNNING, TASK_RUN_STATUS_SUCCEEDED, WORKER_STATUS_STOPPED,
};
use crate::ops::types::{AssignedSession, TaskParameterValue, UpdatedSessionActionInfo};

/// The `DesiredWorkerStatus` the service asks a worker for.
///
/// `STOPPED` is the only value the enum spells
/// (`smithy_query(query="shape", shape="DesiredWorkerStatus")`): the service either wants
/// the worker to stop or has nothing to say about its status.
pub const DESIRED_WORKER_STATUS_STOPPED: &str = "STOPPED";

/// `WorkerStatus` a worker on its way to [`WORKER_STATUS_STOPPED`] carries.
///
/// From `smithy_query(query="shape", shape="WorkerStatus")`. A worker that has reported
/// itself stopping is one whose desired status is stopped, so the schedule gives it no
/// further work and asks back the actions it still holds.
pub const WORKER_STATUS_STOPPING: &str = "STOPPING";

/// How long the service asks a worker to wait before asking for its schedule again.
///
/// AWS documents no value for `updateIntervalSeconds` ("updates the time interval (in
/// seconds) for the schedule"), so this is the emulator's chosen poll interval: short enough
/// that a worker picks up work promptly, long enough that an idle worker is not polling
/// continuously. Nothing ever sleeps for it — it is a number the response carries.
pub const UPDATE_INTERVAL_SECONDS: i32 = 5;

/// What one worker's poll of its schedule answers with.
#[derive(Debug, Default)]
pub struct WorkerSchedule {
    pub assigned_sessions: HashMap<String, AssignedSession>,
    pub cancel_session_actions: HashMap<String, Vec<String>>,
    pub desired_worker_status: Option<String>,
}

/// Where one session lives: the queue and the job it hangs off, and its own id.
type SessionLocation = (String, String, String);

/// One task waiting for a worker: the step it belongs to, its id, and what it runs with.
type ReadyTask = (String, String, HashMap<String, TaskParameterValue>);

impl ComAmazonawsDeadlineDeadlineState {
    /// The job `job_id` names UNDER `farm_id`/`queue_id`, mutably.
    pub fn job_mut(&mut self, farm_id: &str, queue_id: &str, job_id: &str) -> Option<&mut Job> {
        self.farms
            .get_mut(farm_id)?
            .queues
            .get_mut(queue_id)?
            .jobs
            .get_mut(job_id)
    }

    /// Record what `worker_id` reported about the session actions it holds.
    ///
    /// `Err` carries the id of a reported action this worker does not hold: a worker reports
    /// only on the actions the service handed IT, so an id belonging to no session of this
    /// worker is a report the service cannot act on, and nothing of the request is applied.
    pub fn report_session_actions(
        &mut self,
        farm_id: &str,
        worker_id: &str,
        reported: &HashMap<String, UpdatedSessionActionInfo>,
    ) -> Result<(), String> {
        // ONE pass over this worker's own sessions gives every action it holds, so a
        // request that reports several of them costs one walk of state the store already
        // bounds plus a lookup per reported id — not one walk per id.
        let held = self.worker_action_locations(farm_id, worker_id);
        // Located first, so a request naming one action this worker does not hold changes
        // nothing at all. Sorted by id, so several reports in one request are applied in an
        // order that does not depend on how the request's map happened to be laid out.
        let mut located: Vec<(SessionLocation, &String, &UpdatedSessionActionInfo)> =
            Vec::with_capacity(reported.len());
        for (session_action_id, info) in reported {
            let Some(location) = held.get(session_action_id) else {
                return Err(session_action_id.clone());
            };
            located.push((location.clone(), session_action_id, info));
        }
        located.sort_by(|left, right| left.1.cmp(right.1));
        for (location, session_action_id, info) in located {
            self.apply_report(farm_id, &location, session_action_id, info);
        }
        Ok(())
    }

    /// Give `worker_id` what to do next, and answer the schedule it then holds.
    pub fn schedule_worker(
        &mut self,
        farm_id: &str,
        fleet_id: &str,
        worker_id: &str,
    ) -> WorkerSchedule {
        // The schedule is one worker's, so there is nothing to give a worker this fleet does
        // not hold. The handler answers such a request as a 404 before reaching here; this
        // is what makes the store's own rule not depend on that.
        let Some(worker) = self.worker(farm_id, fleet_id, worker_id) else {
            return WorkerSchedule::default();
        };
        // A worker that has reported itself stopping or stopped is one the service wants
        // stopped: it is given no further work, and the actions it still holds are asked
        // back.
        let stopping = matches!(
            worker.status.as_str(),
            WORKER_STATUS_STOPPING | WORKER_STATUS_STOPPED
        );

        // Materialise what the clock already owed the resources this schedule reads: the
        // associations that have finished draining, and the jobs whose creation or
        // reconciliation has come due.
        let scheduling_queues = self.scheduling_queues(farm_id, fleet_id);
        for queue_id in &scheduling_queues {
            self.settle_jobs(farm_id, queue_id);
        }

        self.settle_worker_sessions(farm_id, worker_id);
        if !stopping {
            self.assign_next_action(farm_id, fleet_id, worker_id, &scheduling_queues);
        }

        let mut schedule = WorkerSchedule {
            desired_worker_status: stopping.then(|| DESIRED_WORKER_STATUS_STOPPED.to_string()),
            ..WorkerSchedule::default()
        };
        for (queue_id, job_id, session_id) in self.worker_sessions(farm_id, worker_id) {
            let Some(session) = self
                .queue(farm_id, &queue_id)
                .and_then(|queue| queue.jobs.get(&job_id))
                .and_then(|job| job.sessions.get(&session_id))
            else {
                continue;
            };
            if !session.open() {
                continue;
            }
            if stopping {
                let unfinished = session.unfinished_action_ids();
                if !unfinished.is_empty() {
                    schedule.cancel_session_actions.insert(session_id, unfinished);
                }
                continue;
            }
            schedule
                .assigned_sessions
                .insert(session_id, session.assigned());
        }
        schedule
    }

    /// The queues of `farm_id` whose work is scheduled onto `fleet_id` right now.
    ///
    /// Each association's own clock-owed drain is materialised first, so a fleet whose
    /// association has finished stopping is given no further work.
    fn scheduling_queues(&mut self, farm_id: &str, fleet_id: &str) -> Vec<String> {
        let Some(farm) = self.farms.get_mut(farm_id) else {
            return Vec::new();
        };
        let mut queues = Vec::new();
        for queue in farm.queues.values_mut() {
            if let Some(association) = queue.fleet_associations.get_mut(fleet_id) {
                association.settle_status();
                if association.status == QUEUE_FLEET_ASSOCIATION_STATUS_ACTIVE {
                    queues.push(queue.queue_id.clone());
                }
            }
        }
        queues.sort();
        queues
    }

    /// Every session of `worker_id` under `farm_id`, wherever it hangs.
    fn worker_sessions(&self, farm_id: &str, worker_id: &str) -> Vec<SessionLocation> {
        let Some(farm) = self.farms.get(farm_id) else {
            return Vec::new();
        };
        let mut located: Vec<SessionLocation> = farm
            .queues
            .values()
            .flat_map(|queue| queue.jobs.values())
            .flat_map(|job| job.sessions.values())
            .filter(|session| session.worker_id == worker_id)
            .map(|session| {
                (
                    session.queue_id.clone(),
                    session.job_id.clone(),
                    session.session_id.clone(),
                )
            })
            .collect();
        located.sort();
        located
    }

    /// Where every session action `worker_id` holds lives, by action id.
    ///
    /// The one walk of the farm's sessions a report of this worker's actions needs: a
    /// worker reports only on actions of its own sessions, so what this holds is bounded by
    /// what the store already bounds ([`super::MAX_SESSIONS_PER_JOB`] sessions of at most
    /// [`super::MAX_SESSION_ACTIONS`] actions, per job).
    fn worker_action_locations(
        &self,
        farm_id: &str,
        worker_id: &str,
    ) -> HashMap<String, SessionLocation> {
        let mut held: HashMap<String, SessionLocation> = HashMap::new();
        let Some(farm) = self.farms.get(farm_id) else {
            return held;
        };
        for session in farm
            .queues
            .values()
            .flat_map(|queue| queue.jobs.values())
            .flat_map(|job| job.sessions.values())
            .filter(|session| session.worker_id == worker_id)
        {
            for action in &session.actions {
                held.insert(
                    action.session_action_id.clone(),
                    (
                        session.queue_id.clone(),
                        session.job_id.clone(),
                        session.session_id.clone(),
                    ),
                );
            }
        }
        held
    }

    /// Apply one reported action to the action itself, to the task it ran, and to the job.
    fn apply_report(
        &mut self,
        farm_id: &str,
        location: &SessionLocation,
        session_action_id: &str,
        info: &UpdatedSessionActionInfo,
    ) {
        let (queue_id, job_id, session_id) = location;
        let Some(job) = self.job_mut(farm_id, queue_id, job_id) else {
            return;
        };
        let now = SystemTime::now();
        let max_retries = job.max_retries_per_task;

        // What the worker said, recorded on the action it said it about.
        let Some(session) = job.sessions.get_mut(session_id) else {
            return;
        };
        let Some(action) = session.action_mut(session_action_id) else {
            return;
        };
        // An action the worker has already reported finished is finished: a later report of
        // progress on it does not un-finish it, and neither does it move the task it ran.
        if action.completed() && !info.completed_status.as_deref().is_some_and(super::is_completed_status)
        {
            return;
        }
        action.worker_updated_at = Some(reported_at(info.updated_at.as_ref()).unwrap_or(now));
        if let Some(at) = reported_at(info.started_at.as_ref()) {
            action.started_at = Some(at);
        }
        if let Some(percent) = info.progress_percent {
            action.progress_percent = Some(percent);
        }
        // What the worker reports is bounded by the model BEFORE this handler runs, so what
        // one action holds is bounded too: `progressMessage` is `len 0..4096` and
        // `manifests` is `len 0..10` of entries whose `outputManifestPath` is `len 1..512`
        // and `outputManifestHash` `len 1..256`
        // (`smithy_query(query="closure", operation="UpdateWorkerSchedule")`). Each is the
        // LAST thing the worker said about this action, replacing what it said before, so
        // reporting repeatedly grows nothing.
        if let Some(message) = info.progress_message.as_ref() {
            action.progress_message = Some(message.clone());
        }
        if let Some(code) = info.process_exit_code {
            action.process_exit_code = Some(code);
        }
        if let Some(manifests) = info.manifests.as_ref() {
            action.manifests = manifests
                .iter()
                .map(|manifest| crate::ops::types::TaskRunManifestPropertiesResponse {
                    output_manifest_path: manifest.output_manifest_path.clone(),
                    output_manifest_hash: manifest.output_manifest_hash.clone(),
                })
                .collect();
        }
        match info
            .completed_status
            .as_deref()
            .filter(|status| super::is_completed_status(status))
        {
            // A report carrying a completed status is the worker saying it is done with the
            // action, however it went.
            Some(status) => {
                action.status = status.to_string();
                action.ended_at = Some(reported_at(info.ended_at.as_ref()).unwrap_or(now));
            }
            // A report without one is progress: the worker is working on it.
            None => action.status = SESSION_ACTION_STATUS_RUNNING.to_string(),
        }
        // A worker that says nothing about when it started is recorded as having started
        // when the service first heard from it about the action, so an action the worker
        // reported nothing about carries no start at all.
        if action.started_at.is_none() {
            action.started_at = Some(now);
        }
        let outcome = action.status.clone();
        let started_at = action.started_at;
        let ended_at = action.ended_at;
        let kind = action.kind.clone();
        session.updated_at = Some(now);

        // What that does to the task the action ran, if it ran one.
        if let SessionActionKind::TaskRun {
            step_id, task_id, ..
        } = kind
        {
            if let Some(task) = job.step_mut(&step_id).and_then(|step| {
                step.tasks
                    .iter_mut()
                    .find(|task| task.task_id == task_id)
            }) {
                task.latest_session_action_id = Some(session_action_id.to_string());
                task.updated_at = Some(now);
                if task.started_at.is_none() {
                    task.started_at = started_at;
                }
                match outcome.as_str() {
                    COMPLETED_STATUS_SUCCEEDED => {
                        task.run_status = TASK_RUN_STATUS_SUCCEEDED.to_string();
                        task.ended_at = ended_at.or(Some(now));
                    }
                    COMPLETED_STATUS_FAILED => {
                        if task.failure_retry_count < max_retries {
                            // The job allows another attempt, so the task goes back to
                            // waiting for a worker and the attempt is charged.
                            task.failure_retry_count = task.failure_retry_count.saturating_add(1);
                            task.run_status = TASK_RUN_STATUS_READY.to_string();
                            task.ended_at = None;
                        } else {
                            task.run_status = TASK_RUN_STATUS_FAILED.to_string();
                            task.ended_at = ended_at.or(Some(now));
                        }
                    }
                    COMPLETED_STATUS_CANCELED => {
                        task.run_status = TASK_RUN_STATUS_CANCELED.to_string();
                        task.ended_at = ended_at.or(Some(now));
                    }
                    // The task was not run, so nothing about it was decided: it goes back
                    // to waiting, and no retry is charged for a run that never happened.
                    COMPLETED_STATUS_INTERRUPTED | COMPLETED_STATUS_NEVER_ATTEMPTED => {
                        task.run_status = TASK_RUN_STATUS_READY.to_string();
                        task.ended_at = None;
                    }
                    _ => task.run_status = TASK_RUN_STATUS_RUNNING.to_string(),
                }
            }
        }
        job.settle_run_status_from_tasks();
    }

    /// Finish off every session of `worker_id` that has nothing left to do: a session whose
    /// actions are all finished exits the environments it entered, and ends once it has.
    fn settle_worker_sessions(&mut self, farm_id: &str, worker_id: &str) {
        for (queue_id, job_id, session_id) in self.worker_sessions(farm_id, worker_id) {
            let Some(job) = self
                .queue(farm_id, &queue_id)
                .and_then(|queue| queue.jobs.get(&job_id))
            else {
                continue;
            };
            let Some(session) = job.sessions.get(&session_id) else {
                continue;
            };
            if !session.open() || session.working() {
                continue;
            }
            // A session keeps the environments it entered, so it takes another task of the
            // same step rather than ending while one is waiting — until it has held as many
            // actions as a session may hold.
            if !session.full() && next_ready_task(job, session.step_id.as_deref()).is_some() {
                continue;
            }
            let Some(job) = self.job_mut(farm_id, &queue_id, &job_id) else {
                continue;
            };
            let Some(session) = job.sessions.get_mut(&session_id) else {
                continue;
            };
            if !session.entered_environments.is_empty() {
                let exits: Vec<String> = session.entered_environments.drain(..).rev().collect();
                for environment_id in exits {
                    session.push_action(SessionActionKind::EnvironmentExit { environment_id });
                }
                continue;
            }
            session.lifecycle_status = SESSION_LIFECYCLE_ENDED.to_string();
            session.ended_at = Some(SystemTime::now());
            session.target_lifecycle_status = None;
        }
    }

    /// Put the next task on `worker_id`: another one in the session it already holds, or a
    /// new session on the job that is next to be scheduled.
    fn assign_next_action(
        &mut self,
        farm_id: &str,
        fleet_id: &str,
        worker_id: &str,
        queues: &[String],
    ) {
        if let Some((queue_id, job_id, session_id)) = self
            .worker_sessions(farm_id, worker_id)
            .into_iter()
            .find(|(queue_id, job_id, session_id)| {
                self.queue(farm_id, queue_id)
                    .and_then(|queue| queue.jobs.get(job_id))
                    .and_then(|job| job.sessions.get(session_id))
                    .is_some_and(Session::open)
            })
        {
            // The worker already holds a session: it is given more work only once it has
            // finished what it holds, and only from the step that session is running.
            let Some(job) = self
                .queue(farm_id, &queue_id)
                .and_then(|queue| queue.jobs.get(&job_id))
            else {
                return;
            };
            let Some(session) = job.sessions.get(&session_id) else {
                return;
            };
            if session.working() || session.full() {
                return;
            }
            let Some(ready) = next_ready_task(job, session.step_id.as_deref()) else {
                return;
            };
            self.hand_task_over(farm_id, &queue_id, &job_id, &session_id, ready);
            return;
        }

        let Some((queue_id, job_id, ready, environments)) = self.next_assignment(farm_id, queues)
        else {
            return;
        };
        let session_id = self.mint_id("session");
        let Some(job) = self.job_mut(farm_id, &queue_id, &job_id) else {
            return;
        };
        let mut session = Session::new(
            session_id.clone(),
            farm_id,
            &queue_id,
            &job_id,
            fleet_id,
            worker_id,
        );
        // A job with attachments has them synchronised onto the host before anything runs.
        if job.attachments.is_some() {
            session.push_action(SessionActionKind::SyncInputJobAttachments {
                step_id: Some(ready.0.clone()),
            });
        }
        for environment_id in environments {
            session
                .entered_environments
                .push(environment_id.clone());
            session.push_action(SessionActionKind::EnvironmentEnter { environment_id });
        }
        session.step_id = Some(ready.0.clone());
        job.sessions.insert(session_id.clone(), session);
        self.hand_task_over(farm_id, &queue_id, &job_id, &session_id, ready);
    }

    /// Hand one waiting task to an existing session, and mark it as that session's.
    fn hand_task_over(
        &mut self,
        farm_id: &str,
        queue_id: &str,
        job_id: &str,
        session_id: &str,
        ready: ReadyTask,
    ) {
        let (step_id, task_id, parameters) = ready;
        let Some(job) = self.job_mut(farm_id, queue_id, job_id) else {
            return;
        };
        let Some(session) = job.sessions.get_mut(session_id) else {
            return;
        };
        let session_action_id = session.push_action(SessionActionKind::TaskRun {
            step_id: step_id.clone(),
            task_id: task_id.clone(),
            parameters,
        });
        if let Some(task) = job
            .step_mut(&step_id)
            .and_then(|step| step.tasks.iter_mut().find(|task| task.task_id == task_id))
        {
            task.run_status = TASK_RUN_STATUS_ASSIGNED.to_string();
            task.latest_session_action_id = Some(session_action_id);
            task.updated_at = Some(SystemTime::now());
        }
        job.settle_run_status_from_tasks();
    }

    /// The job to schedule next out of `queues`, the task of it that is waiting, and the
    /// environments a session running that task enters.
    ///
    /// The jobs are ordered the way the service documents `priority`, and the first one that
    /// has a waiting task this emulator can place is taken.
    fn next_assignment(
        &self,
        farm_id: &str,
        queues: &[String],
    ) -> Option<(String, String, ReadyTask, Vec<String>)> {
        let mut candidates: Vec<&Job> = Vec::new();
        for queue_id in queues {
            let Some(queue) = self.queue(farm_id, queue_id) else {
                continue;
            };
            for job in queue.jobs.values() {
                if job.task_run_status != TASK_RUN_STATUS_READY || !job.assignable() {
                    continue;
                }
                // What the scheduler may store for one job is bounded, however many times
                // its tasks are run (see `super::MAX_SESSIONS_PER_JOB`).
                if job.sessions.len() >= super::MAX_SESSIONS_PER_JOB {
                    continue;
                }
                // `maxWorkerCount` is how many workers may process the job at once; -1 is
                // no maximum, and the member is absent for a job the service does not
                // throttle at all (`smithy_query(query="doc", shape="CreateJobRequest",
                // member="maxWorkerCount")`).
                if let Some(max) = job.max_worker_count.filter(|max| *max >= 0) {
                    let holding = job
                        .sessions
                        .values()
                        .filter(|session| session.open())
                        .count();
                    if holding >= max.max(0) as usize {
                        continue;
                    }
                }
                candidates.push(job);
            }
        }
        candidates.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then(left.created_at.cmp(&right.created_at))
                .then(left.job_id.cmp(&right.job_id))
        });
        candidates.into_iter().find_map(|job| {
            let ready = next_ready_task(job, None)?;
            let environments = self.session_environments(farm_id, job, &ready.0)?;
            Some((
                job.queue_id.clone(),
                job.job_id.clone(),
                ready,
                environments,
            ))
        })
    }

    /// The environments, in the order they are entered, that a session running a task of
    /// `step_id` of `job` enters: its queue's, in the priority order the service documents,
    /// then the job template's, then the step's.
    ///
    /// `None` when one of those environments carries a name no `EnvironmentId` can hold, so
    /// the session could not name it to the worker.
    fn session_environments(
        &self,
        farm_id: &str,
        job: &Job,
        step_id: &str,
    ) -> Option<Vec<String>> {
        let mut ids = Vec::new();
        if let Some(queue) = self.queue(farm_id, &job.queue_id) {
            // `CreateQueueEnvironment` documents `priority` as "0 to 10,000, where 0 is the
            // highest priority (activated first and deactivated last). If two environments
            // share the same priority value, the environment created first takes higher
            // priority".
            let mut environments: Vec<&super::QueueEnvironment> =
                queue.environments.values().collect();
            environments.sort_by(|left, right| {
                left.priority
                    .cmp(&right.priority)
                    .then(left.created_at.cmp(&right.created_at))
                    .then(left.queue_environment_id.cmp(&right.queue_environment_id))
            });
            for environment in environments {
                ids.push(super::environment_id(
                    ENVIRONMENT_SCOPE_JOB,
                    &job.job_id,
                    &environment.name,
                )?);
            }
        }
        for name in &job.environments {
            ids.push(super::environment_id(
                ENVIRONMENT_SCOPE_JOB,
                &job.job_id,
                name,
            )?);
        }
        let step = job.steps.iter().find(|step| step.step_id == step_id)?;
        for name in &step.environments {
            ids.push(super::environment_id(
                ENVIRONMENT_SCOPE_STEP,
                step_id,
                name,
            )?);
        }
        Some(ids)
    }
}

/// A timestamp the worker reported, as a stored instant.
fn reported_at(reported: Option<&::aws_smithy_types::DateTime>) -> Option<SystemTime> {
    SystemTime::try_from(*reported?).ok()
}

/// The next task of `job` that is waiting for a worker, from `step_id` when a session is
/// already running that step.
fn next_ready_task(job: &Job, step_id: Option<&str>) -> Option<ReadyTask> {
    if job.task_run_status != TASK_RUN_STATUS_READY || !job.assignable() {
        return None;
    }
    job.steps
        .iter()
        .filter(|step| step_id.is_none_or(|wanted| step.step_id == wanted))
        .find_map(|step| {
            step.tasks
                .iter()
                .find(|task| task.schedulable())
                .map(|task| {
                    (
                        step.step_id.clone(),
                        task.task_id.clone(),
                        task.parameters.clone(),
                    )
                })
        })
}
