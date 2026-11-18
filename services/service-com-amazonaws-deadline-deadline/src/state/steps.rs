// SPDX-License-Identifier: CC0-1.0

//! The steps and tasks a job holds: what its template declared, and what running them does
//! to the job's own reported status.
//!
//! The work hangs off its job exactly as the model's paths nest it
//! (`/2023-10-12/farms/{farmId}/queues/{queueId}/jobs/{jobId}/steps/{stepId}/tasks/{taskId}`),
//! so a step is reachable only through its job and a task only through its step. The ids
//! follow the same nesting, which is what the modeled label patterns spell: a step's id is
//! `step-` plus 32 hex characters, and its tasks are that same hex with the index of the
//! task appended (`^task-[0-9a-f]{32}-(0|([1-9][0-9]{0,9}))$`), so a task id names the step
//! it belongs to.
//!
//! The steps are minted when the job is created, from the work
//! [`super::job_template_work`] read out of its template: a job's steps exist from the
//! moment the job does, not from the moment a worker first asks for work.

use std::collections::HashMap;
use std::time::SystemTime;

use super::{
    ComAmazonawsDeadlineDeadlineState, DeclaredWork, Job, TASK_RUN_STATUS_PENDING,
    TASK_RUN_STATUS_READY,
};
use crate::ops::types::TaskParameterValue;

/// `StepLifecycleStatus` a step carries once its job has been assembled.
///
/// From `smithy_query(query="shape", shape="StepLifecycleStatus")`: `CREATE_COMPLETE`,
/// `UPDATE_IN_PROGRESS`, `UPDATE_FAILED`, `UPDATE_SUCCEEDED`. A step is created with its
/// job and nothing updates it, so it stays here.
pub const STEP_LIFECYCLE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `TaskRunStatus` a task carries from the moment the scheduler puts it on a worker until
/// that worker reports it started.
///
/// From `smithy_query(query="shape", shape="TaskRunStatus")`: `PENDING`, `READY`,
/// `ASSIGNED`, `STARTING`, `SCHEDULED`, `INTERRUPTING`, `RUNNING`, `SUSPENDED`, `CANCELED`,
/// `FAILED`, `SUCCEEDED`, `NOT_COMPATIBLE`.
pub const TASK_RUN_STATUS_ASSIGNED: &str = "ASSIGNED";
/// `TaskRunStatus` a task carries while the worker it is assigned to is running it.
pub const TASK_RUN_STATUS_RUNNING: &str = "RUNNING";
/// `TaskRunStatus` a task that ran to completion carries.
pub const TASK_RUN_STATUS_SUCCEEDED: &str = "SUCCEEDED";
/// `TaskRunStatus` a task whose runs have all failed carries.
pub const TASK_RUN_STATUS_FAILED: &str = "FAILED";
/// `TaskRunStatus` a task that was cancelled rather than run carries.
pub const TASK_RUN_STATUS_CANCELED: &str = "CANCELED";
/// `TaskRunStatus` a task held back from scheduling carries.
pub const TASK_RUN_STATUS_SUSPENDED: &str = "SUSPENDED";

/// One task of a step: one point of the step's parameter space, and how its runs have gone.
#[derive(Debug, Clone)]
pub struct Task {
    pub task_id: String,
    /// The parameters this task runs with, as the step's parameter space enumerated them.
    pub parameters: HashMap<String, TaskParameterValue>,
    pub run_status: String,
    /// How many times a run of this task has failed and been re-queued. `GetTask` reports
    /// it as `failureRetryCount`.
    pub failure_retry_count: i32,
    /// The session action of the most recent run of this task, `None` until one is assigned.
    pub latest_session_action_id: Option<String>,
    pub started_at: Option<SystemTime>,
    pub ended_at: Option<SystemTime>,
    pub updated_at: Option<SystemTime>,
}

impl Task {
    /// The task at `index` of the step `step_id`, carrying `parameters`.
    ///
    /// A task's id is its step's hex with the index appended, which is what the modeled
    /// `TaskId` pattern spells.
    fn new(step_id: &str, index: usize, parameters: HashMap<String, TaskParameterValue>) -> Self {
        let hex = step_id.strip_prefix("step-").unwrap_or(step_id);
        Self {
            task_id: format!("task-{hex}-{index}"),
            parameters,
            // The job is still being assembled when its tasks are minted, so nothing can
            // be scheduled yet; the move to the submitted target comes with the job's own
            // (see `Job::settle`).
            run_status: TASK_RUN_STATUS_PENDING.to_string(),
            failure_retry_count: 0,
            latest_session_action_id: None,
            started_at: None,
            ended_at: None,
            updated_at: None,
        }
    }

    /// Whether this task is waiting for a worker.
    pub fn schedulable(&self) -> bool {
        self.run_status == TASK_RUN_STATUS_READY
    }

    /// Whether this task has finished, however it finished.
    pub fn terminal(&self) -> bool {
        matches!(
            self.run_status.as_str(),
            TASK_RUN_STATUS_SUCCEEDED | TASK_RUN_STATUS_FAILED | TASK_RUN_STATUS_CANCELED
        )
    }

    /// Whether a worker is holding this task right now.
    pub fn in_flight(&self) -> bool {
        matches!(
            self.run_status.as_str(),
            TASK_RUN_STATUS_ASSIGNED | TASK_RUN_STATUS_RUNNING
        )
    }
}

/// One step of a job: the work its template declared under a `steps` entry.
#[derive(Debug, Clone)]
pub struct Step {
    pub step_id: String,
    pub name: String,
    pub lifecycle_status: String,
    /// The names of the step's own environments, in declared order.
    pub environments: Vec<String>,
    /// The step's tasks, in the order its parameter space enumerates them.
    pub tasks: Vec<Task>,
}

/// What one task costs the budgets that bound this store's work, beside its parameters: a
/// flat figure for its id and its record, so a task that carries no parameter costs
/// something too.
pub const TASK_BYTES: usize = 128;

/// What one task parameter costs those budgets: the key and the value text stored for it.
///
/// Every member of `TaskParameterValue` is a string on the wire
/// (`smithy_query(query="shape", shape="TaskParameterValue")`), so what a parameter costs is
/// the length of its name and of that string.
pub fn task_parameter_bytes(name: &str, value: &TaskParameterValue) -> usize {
    let text = match value {
        TaskParameterValue::Int(text)
        | TaskParameterValue::Float(text)
        | TaskParameterValue::String(text)
        | TaskParameterValue::Path(text)
        | TaskParameterValue::chunkInt(text) => text.len(),
    };
    name.len().saturating_add(text)
}

/// What the work in `steps` costs the store's budget for it
/// ([`super::MAX_STORED_TASK_BYTES`]): every task's parameters, plus [`TASK_BYTES`] per
/// task.
pub fn stored_task_bytes(steps: &[Step]) -> usize {
    steps
        .iter()
        .flat_map(|step| step.tasks.iter())
        .map(|task| {
            task.parameters
                .iter()
                .map(|(name, value)| task_parameter_bytes(name, value))
                .fold(TASK_BYTES, usize::saturating_add)
        })
        .fold(0, usize::saturating_add)
}

impl ComAmazonawsDeadlineDeadlineState {
    /// Mint the steps and tasks of a job from the work its template declared.
    pub fn mint_steps(&mut self, work: &DeclaredWork) -> Vec<Step> {
        let mut steps = Vec::with_capacity(work.steps.len());
        for declared in &work.steps {
            let step_id = self.mint_id("step");
            let tasks = declared
                .tasks
                .iter()
                .enumerate()
                .map(|(index, parameters)| Task::new(&step_id, index, parameters.clone()))
                .collect();
            steps.push(Step {
                step_id,
                name: declared.name.clone(),
                lifecycle_status: STEP_LIFECYCLE_CREATE_COMPLETE.to_string(),
                environments: declared.environments.clone(),
                tasks,
            });
        }
        steps
    }

    /// Mint the steps of a job created FROM another job: the same work under ids of its
    /// own, with nothing of the source job's run carried over.
    ///
    /// The source job's template was read when IT was created, so the document is not
    /// parsed again — the work is copied from what that read stored.
    pub fn copy_steps(&mut self, steps: &[Step]) -> Vec<Step> {
        let mut copied = Vec::with_capacity(steps.len());
        for step in steps {
            let step_id = self.mint_id("step");
            let tasks = step
                .tasks
                .iter()
                .enumerate()
                .map(|(index, task)| Task::new(&step_id, index, task.parameters.clone()))
                .collect();
            copied.push(Step {
                step_id,
                name: step.name.clone(),
                lifecycle_status: STEP_LIFECYCLE_CREATE_COMPLETE.to_string(),
                environments: step.environments.clone(),
                tasks,
            });
        }
        copied
    }
}

impl Job {
    /// Every task of this job, in step order.
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.steps.iter().flat_map(|step| step.tasks.iter())
    }

    /// Every task of this job, mutably, in step order.
    pub fn tasks_mut(&mut self) -> impl Iterator<Item = &mut Task> {
        self.steps.iter_mut().flat_map(|step| step.tasks.iter_mut())
    }

    /// How many of this job's tasks carry each run status, or `None` for a job that holds no
    /// task at all — a job whose template declared no step has no counts to report, which
    /// is not the same as reporting zero of each.
    pub fn task_run_status_counts(&self) -> Option<HashMap<String, i32>> {
        let mut counts: HashMap<String, i32> = HashMap::new();
        for task in self.tasks() {
            *counts.entry(task.run_status.clone()).or_default() += 1;
        }
        (!counts.is_empty()).then_some(counts)
    }

    /// How many times a run of one of this job's tasks has failed and been re-queued.
    ///
    /// Absent for a job that holds no step, which has no run to count retries of.
    pub fn task_failure_retry_count(&self) -> Option<i32> {
        (!self.steps.is_empty()).then(|| {
            self.tasks()
                .map(|task| task.failure_retry_count)
                .fold(0, i32::saturating_add)
        })
    }

    /// The step `step_id` names, mutably.
    pub fn step_mut(&mut self, step_id: &str) -> Option<&mut Step> {
        self.steps.iter_mut().find(|step| step.step_id == step_id)
    }

    /// Settle this job's own run status, and its start and end, from what its tasks now
    /// carry.
    ///
    /// Called where a task's status changed — a worker reporting what it did with one — so
    /// every reader of the job reports the run the tasks are actually in. A job that holds
    /// no task keeps the status its creation settled it to: there is nothing to aggregate.
    pub fn settle_run_status_from_tasks(&mut self) {
        let tasks: Vec<&Task> = self.tasks().collect();
        let Some(aggregate) = aggregate_run_status(&tasks) else {
            return;
        };
        // The job's own failure budget stops the run before its remaining tasks are
        // scheduled: `GetJob` documents `maxFailedTasksCount` as "the number of task
        // failures before the job stops running and is marked as FAILED", so a job that
        // allows none fails on its first failure.
        let failed = tasks
            .iter()
            .filter(|task| task.run_status == TASK_RUN_STATUS_FAILED)
            .count();
        let budget = self.max_failed_tasks_count.max(1) as usize;
        let status = if failed >= budget {
            TASK_RUN_STATUS_FAILED.to_string()
        } else {
            aggregate
        };
        let now = SystemTime::now();
        if self.tasks().any(|task| task.started_at.is_some()) && self.started_at.is_none() {
            self.started_at = Some(now);
        }
        let ended = matches!(
            status.as_str(),
            TASK_RUN_STATUS_SUCCEEDED | TASK_RUN_STATUS_FAILED | TASK_RUN_STATUS_CANCELED
        );
        if ended && self.ended_at.is_none() {
            self.ended_at = Some(now);
        }
        if !ended {
            self.ended_at = None;
        }
        self.task_run_status = status;
    }
}

/// The run status a set of tasks has settled to, or `None` when the set is empty.
///
/// What the job reports:
///
/// * every task succeeded, and the run succeeded;
/// * a worker is holding a task, or a task is still waiting for one, and the run is still
///   going however its finished tasks went;
/// * every task has finished and some did not succeed, so the run carries what stopped it;
/// * otherwise the run carries what its tasks are waiting as.
///
/// A job's own failure budget is NOT applied here — a step does not carry one — so
/// [`Job::settle_run_status_from_tasks`] applies it to the job on top of this.
fn aggregate_run_status(tasks: &[&Task]) -> Option<String> {
    if tasks.is_empty() {
        return None;
    }
    let count = |status: &str| {
        tasks
            .iter()
            .filter(|task| task.run_status == status)
            .count()
    };
    let total = tasks.len();
    let succeeded = count(TASK_RUN_STATUS_SUCCEEDED);
    let failed = count(TASK_RUN_STATUS_FAILED);
    let not_compatible = count(super::TASK_RUN_STATUS_NOT_COMPATIBLE);
    let terminal = tasks.iter().filter(|task| task.terminal()).count();
    let in_flight = tasks.iter().filter(|task| task.in_flight()).count();
    let ready = tasks.iter().filter(|task| task.schedulable()).count();
    let suspended = count(TASK_RUN_STATUS_SUSPENDED);

    let status = if succeeded == total {
        TASK_RUN_STATUS_SUCCEEDED
    } else if not_compatible == total {
        super::TASK_RUN_STATUS_NOT_COMPATIBLE
    } else if in_flight > 0 {
        TASK_RUN_STATUS_RUNNING
    } else if ready > 0 {
        TASK_RUN_STATUS_READY
    } else if terminal == total {
        if failed > 0 {
            TASK_RUN_STATUS_FAILED
        } else {
            TASK_RUN_STATUS_CANCELED
        }
    } else if suspended > 0 {
        TASK_RUN_STATUS_SUSPENDED
    } else {
        TASK_RUN_STATUS_PENDING
    };
    Some(status.to_string())
}
