// SPDX-License-Identifier: CC0-1.0

//! When a JOB may be deleted, and how a delete that may not proceed is refused.
//!
//! `DeleteJob$force` is read against the JOB's own status: "you can only delete a job
//! which is in a terminal state ("COMPLETED" or "CANCELED") or an exception will occur",
//! and the exception the model names for "deleting a job … which is IN_PROGRESS without
//! setting the force parameter" is `InvalidStateTransitionException`. So the status the
//! job is REPORTED in ([`super::job_progress`]) decides, and the refusal states it — a
//! client reads the status it was refused in out of the sentence.

/// How this plane refuses a `DeleteJob` of a job that has not reached a status it may be
/// deleted in.
///
/// The fixed text is the jobs plane's sentence for that refusal, and its two variable
/// parts are the `jobId` the request named and the `JobStatus` the job is reported in at
/// the moment of the refusal — the status whose non-terminality is the reason, which is
/// why it is named rather than any count of the job's executions.
pub fn undeletable_job_status_message(job_id: &str, status: &str) -> String {
    format!("Job {job_id} in status {status} cannot be deleted without force flag.")
}
