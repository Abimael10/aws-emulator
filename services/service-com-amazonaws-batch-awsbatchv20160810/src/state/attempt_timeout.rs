// SPDX-License-Identifier: CC0-1.0

//! The ATTEMPT TIMEOUT a submitted job can carry, and what expiring it does.
//!
//! # The rule this module implements
//!
//! `JobTimeout$attemptDurationSeconds` is "the job timeout time (in seconds) that's measured
//! from the job attempt's `startedAt` timestamp. After this time passes, Batch terminates
//! your jobs if they aren't finished" — so the timeout is a DEADLINE on the attempt the
//! scheduler started, armed at the instant that attempt entered `RUNNING`
//! (`AttemptDetail$startedAt`: "when the attempt transitioned from the `STARTING` state to
//! the `RUNNING` state") and falling due `attemptDurationSeconds` later. When it falls due
//! the attempt is ended by the SERVICE rather than by the work: the job reaches the terminal
//! `FAILED`, reports the instant of the deadline as its `stoppedAt` ("when the job
//! transitioned from the `RUNNING` state to a terminal state") and carries the
//! `statusReason` of that failure class, which the attempt it ended reports too
//! ([`ATTEMPT_TIMEOUT_REASON`]).
//!
//! A job the timeout never reaches is left to whatever its work comes to: an attempt that
//! has not started has no instant to measure from, and a job that finishes inside its
//! timeout ends on its own outcome. Nothing here shortens or lengthens the span a job's work
//! takes — it only ends an attempt the caller's own deadline has already run out on.
//!
//! # When it is noticed
//!
//! On the CLOCK, like every other transition this service owes
//! ([`ComAmazonawsBatchAwsbatchv20160810State::advance_job`] checks it before anything
//! else), and the instant the job reports is the DEADLINE rather than the instant the pass
//! happened to run — so two readers cannot see one timed-out job end at two instants. The
//! deadline also BOUNDS the next instant the clock owes a running job
//! ([`ComAmazonawsBatchAwsbatchv20160810State::set_job_due_bounded_by_attempt_deadline`]),
//! so a job whose next step is further off than its own timeout is still ended when that
//! timeout runs out and not when its step was due.

use super::*;

/// The `statusReason` of a job whose attempt ran out of the time its `timeout` allowed it.
///
/// Fixed text with no variable part: it names the failure class — the attempt's duration
/// exceeding the timeout the request asked for — and nothing of the job, the way AWS Batch
/// words the end of an attempt it terminated for its timeout.
pub const ATTEMPT_TIMEOUT_REASON: &str = "Job attempt duration exceeded timeout";

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The instant `id`'s current attempt runs out of the time its `timeout` allows it, or
    /// `None` when nothing bounds it.
    ///
    /// `None` for a job in a terminal status (there is no attempt left to terminate), for one
    /// that stated no `timeout.attemptDurationSeconds`, and for one whose attempt has not
    /// STARTED — the member is "measured from the job attempt's `startedAt` timestamp", so an
    /// attempt with no such instant has nothing to measure from and a job still waiting to be
    /// placed is never timed out.
    ///
    /// Measured from the attempt that is still OPEN where there is one, so a later attempt
    /// gets the whole duration over again rather than inheriting the first one's deadline, and
    /// from the job's own `startedAt` otherwise — which is the same instant while a job has
    /// had exactly one attempt, the only shape this lifecycle produces today (nothing
    /// re-queues a job for a second attempt).
    pub(super) fn attempt_deadline(&self, id: &str) -> Option<i64> {
        let job = self.jobs.get(id)?;
        if is_terminal_status(&job.status) {
            return None;
        }
        let seconds = job.timeout.as_ref()?.attempt_duration_seconds?;
        if seconds <= 0 {
            return None;
        }
        let started_at = job
            .attempts
            .last()
            .filter(|attempt| attempt.ended_at.is_none())
            .and_then(|attempt| attempt.started_at)
            .or(job.started_at)?;
        Some(started_at.saturating_add(i64::from(seconds).saturating_mul(1_000)))
    }

    /// Point `id`'s clock-owed transition at `due`, or at its attempt's deadline when that
    /// falls first.
    ///
    /// Used wherever a job that is RUNNING its attempt is given its next instant: the timeout
    /// is a deadline on the attempt, so it has to be able to end the job before whatever step
    /// was owed next — otherwise a job whose next step is further off than its own timeout
    /// would outlive the deadline by the length of that step.
    pub(super) fn set_job_due_bounded_by_attempt_deadline(&mut self, id: &str, due: i64) {
        let bounded = match self.attempt_deadline(id) {
            Some(deadline) => due.min(deadline),
            None => due,
        };
        self.set_job_due(id, Some(bounded));
    }

    /// End `id` at `deadline`, the instant its attempt ran out of the time its `timeout`
    /// allowed it.
    ///
    /// "Batch terminates your jobs if they aren't finished": the job was TERMINATED, which is
    /// what `JobDetail$isTerminated` ("indicates whether the job is terminated") reports, and
    /// it is put in the terminal `FAILED` with [`ATTEMPT_TIMEOUT_REASON`] through the ONE
    /// terminal path ([`Self::finish_job`]) — so the attempt it was running is closed with the
    /// same reason and the same instant, its `stoppedAt` is written, the consumable resources
    /// and the compute capacity it held are released, and the task it was running as is
    /// stopped with it ([`super::ecs_task`], which ends the task of a job that has gone
    /// terminal).
    pub(super) fn time_out_attempt(&mut self, id: &str, deadline: i64) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.is_terminated = true;
        }
        self.finish_job(id, JOB_FAILED, Some(ATTEMPT_TIMEOUT_REASON), deadline);
    }
}
