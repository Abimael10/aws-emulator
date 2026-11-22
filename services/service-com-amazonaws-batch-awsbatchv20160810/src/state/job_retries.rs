// SPDX-License-Identifier: CC0-1.0

//! The RETRY STRATEGY a job carries, and what it decides when one of the job's attempts has
//! failed.
//!
//! # The rule this module implements
//!
//! A failed attempt is not necessarily the end of the job: `RetryStrategy$attempts` is "the
//! number of times to move a job to the `RUNNABLE` status. You can specify between 1 and 10
//! attempts. If the value of `attempts` is greater than one, the job is retried on failure the
//! same number of attempts as the value", so a job whose attempt failed is MOVED BACK to
//! `RUNNABLE` — for another placement, another task and another run — until it has had that
//! many attempts.
//!
//! WHETHER a given failure is retried is what `evaluateOnExit` decides: it is an "array of up
//! to 5 objects that specify the conditions where jobs are retried or failed ... If none of the
//! listed conditions match, then the job is retried" (`RetryStrategy$evaluateOnExit`, and
//! `EvaluateOnExit`'s own doc repeats it). One entry states "the action to take if all of the
//! specified conditions (`onStatusReason`, `onReason`, and `onExitCode`) are met", and its
//! action's "values aren't case sensitive" (`EvaluateOnExit$action`, which is the member that
//! sentence belongs to) — so the conditions are read in the order the list carries them, the
//! FIRST entry all of whose stated conditions are met decides (`RETRY` for another attempt,
//! `EXIT` for the end of the job), and a list where none of them is met falls back to the
//! documented default: the job is retried. Either way the budget is the ceiling: an attempt
//! that fails when the job has already had `attempts` of them ends the job.
//!
//! Each stated condition is a GLOB over one value the failed attempt reports: `onExitCode`
//! matches "the decimal representation of the `ExitCode` returned for a job", `onReason` "the
//! `Reason` returned for a job" and `onStatusReason` "the `StatusReason` returned for a job",
//! and each pattern "can optionally end with an asterisk (*) so that only the start of the
//! string needs to be an exact match" ([`glob_matches`], which compares the rest of the pattern
//! exactly, character for character) — so a pattern without one matches that value and nothing
//! else. A condition naming a value the attempt does not report (an `onExitCode` for an
//! attempt whose container produced no exit status) is not met, because there is nothing for
//! the pattern to match. This store records ONE sentence for a job's `statusReason` and for
//! its container's `reason` ([`ComAmazonawsBatchAwsbatchv20160810State::finish_job`] writes
//! both from the same value), so `onReason` and `onStatusReason` are matched against that one
//! sentence: a condition stating the two with patterns no single sentence satisfies is
//! therefore never met here.
//!
//! # What a retry is
//!
//! Another PASS of the same lifecycle, from the status the strategy names: the attempt that
//! failed is closed exactly as it ended — it keeps its task, its exit code, its instants and
//! the reason it ended, which is what makes `JobDetail$attempts` ("a list of job attempts that
//! are associated with this job") report one entry per execution — and the job re-enters
//! `RUNNABLE`, where the scheduler places it again
//! ([`ComAmazonawsBatchAwsbatchv20160810State::arm_placement`]). The placement that follows
//! makes its OWN task ([`super::placement`]) and its own run — as an Amazon ECS task
//! ([`super::ecs_task`]) or performed in this process ([`super::container_run`]) — so every
//! attempt of a retried job reports its own `taskArn`, its own log stream and the exit status
//! its own run produced. Nothing of the next attempt is carried over from the one that failed:
//! the records that describe a run are cleared with the re-queue
//! ([`ComAmazonawsBatchAwsbatchv20160810State::requeue_job_for_retry`]), while what the job
//! HOLDS of each consumable resource stays charged — the job has not completed, so it gives
//! that quantity back once, at its terminal status.
//!
//! What the registry answered for the job's container image is NOT re-asked: it is the
//! registry's verdict on a reference, not a property of one attempt, so a retried job carries
//! it into the next attempt and a job whose image cannot be fetched spends its budget reporting
//! that fetch, which is the failure its `evaluateOnExit` can match on.
//!
//! # What is never retried
//!
//! A job the CALLER ended (`CancelJob`, `TerminateJob`) and one the service terminated on its
//! own deadline: `JobTimeout$attemptDurationSeconds` is the time after which "Batch TERMINATES
//! your jobs if they aren't finished" ([`super::attempt_timeout`], which records that as
//! `JobDetail$isTerminated`, "indicates whether the job is terminated"), so such a job was
//! terminated rather than failed by its work — the reading this module applies to every
//! terminated or cancelled job. Neither is a job that never had an attempt at all — one whose
//! dependencies failed, or whose queue was deleted under it — because there is no failed
//! execution to repeat. And a job that SUCCEEDED is not retried whatever its strategy says:
//! `attempts` is what the job is "retried on failure" that many times.

use super::*;

/// How many attempts a job whose `retryStrategy` states no `attempts` gets.
///
/// One: the member is "the number of times to move a job to the `RUNNABLE` status", and a job
/// with no strategy at all is moved there once — which is why a failure with no strategy ends
/// the job.
pub const DEFAULT_RETRY_ATTEMPTS: i32 = MIN_RETRY_ATTEMPTS;

/// The `RetryAction` that asks for another attempt. Compared case-insensitively, because the
/// member's "values aren't case sensitive" (`EvaluateOnExit$action`); every other action is
/// `EXIT`, the only other value the enum declares, which ends the job.
const RETRY_ACTION_RETRY: &str = "RETRY";

/// What the ending of one attempt did to the job it belonged to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptEnding {
    /// The job is TERMINAL: this attempt's outcome is the job's own.
    EndedJob,
    /// The job is back in `RUNNABLE` for another attempt, so it has MOVED rather than ended.
    Retried,
}

/// Whether `value` matches the glob `pattern` one `EvaluateOnExit` condition carries.
///
/// The pattern "can optionally end with an asterisk (*) so that only the start of the string
/// needs to be an exact match" (`EvaluateOnExit$onExitCode`, and the same sentence on
/// `onReason` and `onStatusReason`) — so a pattern ending in one matches every value that
/// STARTS with the rest of it, and a pattern without one matches only that exact value.
#[must_use]
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

/// Whether every condition `condition` STATES is met by an attempt that ended with
/// `exit_code` and `reason`.
///
/// "The action to take if all of the specified conditions (`onStatusReason`, `onReason`, and
/// `onExitCode`) are met" (`EvaluateOnExit$action`): a member the entry does not carry states
/// no condition, so it cannot fail to be met, while a member it does carry must match the
/// value the attempt reports — and an attempt reporting no such value (a container that
/// produced no exit status) meets no condition stated over it.
///
/// `reason` is what the attempt ended with, which is the ONE sentence this store records as
/// the job's `statusReason` and as its container's `reason`
/// ([`ComAmazonawsBatchAwsbatchv20160810State::finish_job`]), so both patterns are matched
/// against it.
#[must_use]
pub fn condition_is_met(
    condition: &EvaluateOnExit,
    exit_code: Option<i32>,
    reason: Option<&str>,
) -> bool {
    let exit_code = exit_code.map(|status| status.to_string());
    let met = |pattern: Option<&String>, value: Option<&str>| match pattern {
        None => true,
        Some(pattern) => value.is_some_and(|value| glob_matches(pattern, value)),
    };
    met(condition.on_exit_code.as_ref(), exit_code.as_deref())
        && met(condition.on_reason.as_ref(), reason)
        && met(condition.on_status_reason.as_ref(), reason)
}

/// The `action` of the FIRST condition of `conditions` that is met by an attempt which ended
/// with `exit_code` and `reason`, or `None` when none of them is met.
///
/// The list is read in the order the request carried it, so the first match is what decides —
/// and "if none of the listed conditions match, then the job is retried", which is the caller's
/// to apply ([`ComAmazonawsBatchAwsbatchv20160810State::another_attempt_is_owed`]).
#[must_use]
pub fn matched_action<'conditions>(
    conditions: &'conditions [EvaluateOnExit],
    exit_code: Option<i32>,
    reason: Option<&str>,
) -> Option<&'conditions str> {
    conditions
        .iter()
        .find(|condition| condition_is_met(condition, exit_code, reason))
        .map(|condition| condition.action.as_str())
}

impl Job {
    /// How many attempts this job's `retryStrategy` allows it: "the number of times to move a
    /// job to the `RUNNABLE` status. You can specify between 1 and 10 attempts."
    ///
    /// A job with no strategy, or one whose strategy states no `attempts`, gets
    /// [`DEFAULT_RETRY_ATTEMPTS`]. The value is held inside the range the member publishes —
    /// which `SubmitJob` and `RegisterJobDefinition` already refuse a request outside — so no
    /// stored record can ask for more passes than the member allows.
    fn retry_attempt_budget(&self) -> usize {
        let stated = self
            .retry_strategy
            .as_ref()
            .and_then(|strategy| strategy.attempts)
            .unwrap_or(DEFAULT_RETRY_ATTEMPTS)
            .clamp(MIN_RETRY_ATTEMPTS, MAX_RETRY_ATTEMPTS);
        usize::try_from(stated).unwrap_or(1)
    }
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// Whether the job `id`, whose attempt has just FAILED with `reason`, is owed another
    /// attempt rather than the terminal status.
    ///
    /// The decision is read off the stored record alone — the attempts it has already had, the
    /// strategy it carries and what the attempt that just failed reports — so two callers at
    /// one instant decide alike.
    ///
    /// A job nothing placed has had no attempt to repeat, and a job a CALLER ended, or one
    /// this service terminated on its own deadline, is not a job whose work failed: none of
    /// those is retried.
    fn another_attempt_is_owed(&self, id: &str, reason: Option<&str>) -> bool {
        let Some(job) = self.jobs.get(id) else {
            return false;
        };
        if job.is_cancelled || job.is_terminated || job.cancel_requested.is_some() {
            return false;
        }
        let Some(attempt) = job.attempts.last() else {
            return false;
        };
        if job.attempts.len() >= job.retry_attempt_budget() {
            return false;
        }
        let conditions = job
            .retry_strategy
            .as_ref()
            .and_then(|strategy| strategy.evaluate_on_exit.as_deref())
            .unwrap_or_default();
        match matched_action(conditions, attempt.exit_code, reason) {
            Some(action) => action.eq_ignore_ascii_case(RETRY_ACTION_RETRY),
            // "If none of the listed conditions match, then the job is retried" — which is
            // also what a strategy carrying no conditions at all says, since a job with
            // `attempts` left "is retried on failure".
            None => true,
        }
    }

    /// End the ATTEMPT the job `id` was running, at `at`, with the `status` its work came to
    /// and the `reason` that goes with it — and answer what that did to the job.
    ///
    /// The one place an attempt's outcome is applied, so every execution this service can run
    /// for a job — a program it performed itself ([`super::container_run`]), the Amazon ECS
    /// task the capacity took ([`super::ecs_task`]), and a container initiation that failed
    /// before either ([`super::image_pull`]) — honours the same retry strategy. A failure the
    /// strategy answers with another attempt re-queues the job
    /// ([`Self::requeue_job_for_retry`]); everything else is the job's own ending
    /// ([`Self::finish_job`]).
    pub(super) fn end_job_attempt(
        &mut self,
        id: &str,
        status: &str,
        reason: Option<&str>,
        at: i64,
    ) -> AttemptEnding {
        if status == JOB_FAILED && self.another_attempt_is_owed(id, reason) {
            self.requeue_job_for_retry(id, reason, at);
            return AttemptEnding::Retried;
        }
        self.finish_job(id, status, reason, at);
        AttemptEnding::EndedJob
    }

    /// Move `id` back to `RUNNABLE` at `at` for another attempt, closing the one that failed
    /// with `reason`.
    ///
    /// `attempts` is "the number of times to move a job to the `RUNNABLE` status", so that is
    /// where the job goes: it joins its queue's ordering again and the scheduler places it as
    /// it places every other job waiting there ([`Self::arm_placement`], which decides WHEN
    /// from the capacity behind the queue). The attempt that failed is CLOSED first and is then
    /// left exactly as it ended, carrying its task, its exit code, its instants and its reason,
    /// so the list of attempts the job reports grows by one real execution per pass.
    ///
    /// Everything that describes a RUN is cleared with the move, because the next attempt has
    /// had none yet: the job reports no `startedAt` until its new attempt enters `RUNNING`, its
    /// container reports no exit code, reason, task or log stream until the new placement and
    /// run produce them, and nothing is owed to Amazon ECS for the attempt that is over.
    ///
    /// What the job HOLDS is left exactly as it is, and that is deliberate. A consumable
    /// resource's `resourceType` "indicates whether the resource is available to be re-used
    /// after a job COMPLETES" ([`consumable_claims`]), and a job being retried has not
    /// completed — it is the same job, continuing — so its claim stays charged across the
    /// requeue and is given back once, at the terminal status. That is also what keeps the
    /// quantity of a `NON_REPLENISHABLE` resource from being consumed once per attempt, and
    /// what admits the next attempt without charging the resource twice
    /// ([`Self::consumable_claims_admit`] and [`Self::charge_consumable_claims`] both read the
    /// claim the job already holds). The compute capacity it held follows from its status,
    /// which this move changes, so the vCPUs of the attempt that failed are free for the jobs
    /// waiting on them.
    fn requeue_job_for_retry(&mut self, id: &str, reason: Option<&str>, at: i64) {
        if let Some(job) = self.jobs.get_mut(id) {
            end_open_attempt(job, reason, at);
            job.started_at = None;
            job.status_reason = None;
            job.container_run = None;
            job.task_dispatch = JobTaskDispatch::Waiting;
            job.task_attempted_at = None;
            job.task_arn = None;
            job.task_cluster = None;
            job.unanswered_task_reads = 0;
            if let Some(container) = job.container.as_mut() {
                container.exit_code = None;
                container.reason = None;
                container.task_arn = None;
                container.log_stream_name = None;
                container.network_interfaces = None;
            }
        }
        self.set_job_status(id, JOB_RUNNABLE, at);
        self.arm_placement(id, at);
        // The vCPUs the failed attempt held are what another job may have been waiting for,
        // so the jobs resting on it are owed their placement from this instant — the same
        // release a terminal job makes ([`Self::finish_job`]).
        self.rearm_resting_jobs(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One condition, as a request would carry it.
    fn condition(
        on_exit_code: Option<&str>,
        on_reason: Option<&str>,
        on_status_reason: Option<&str>,
        action: &str,
    ) -> EvaluateOnExit {
        EvaluateOnExit {
            on_exit_code: on_exit_code.map(str::to_string),
            on_reason: on_reason.map(str::to_string),
            on_status_reason: on_status_reason.map(str::to_string),
            action: action.to_string(),
        }
    }

    /// RULE: a pattern ending in an asterisk matches every value that starts with the rest of
    /// it, and one without matches only that exact value.
    ///
    /// The rows differ along what the rule generalises over: whether the pattern carries the
    /// asterisk, how much of the value it covers, and whether the value is an exit code's
    /// decimal representation or a reason sentence.
    #[test]
    fn a_condition_glob_is_a_prefix_only_where_it_ends_with_an_asterisk() {
        let rows: &[(&str, &str, bool)] = &[
            ("7", "7", true),
            ("7", "70", false),
            ("7", "3", false),
            ("7*", "70", true),
            ("7*", "7", true),
            ("7*", "137", false),
            ("*", "anything at all", true),
            ("CannotPull*", "CannotPullContainerError: no such image", true),
            ("CannotPull", "CannotPullContainerError: no such image", false),
            ("Host EC2*", "Essential container in task exited", false),
        ];
        for (pattern, value, wanted) in rows {
            assert_eq!(
                glob_matches(pattern, value),
                *wanted,
                "case {pattern:?} against {value:?}: expected {wanted}"
            );
        }
    }

    /// RULE: a condition is met only when EVERY member it states matches, and the first
    /// condition of the list that is met decides; a list where none is met decides nothing, so
    /// the documented default (retry) applies.
    #[test]
    fn the_first_condition_whose_stated_members_all_match_decides() {
        const EXITED: &str = "Essential container in task exited";
        /// One row: the conditions a job carries, what its attempt reported, and the action
        /// the list then decides.
        struct Row {
            case: &'static str,
            conditions: Vec<EvaluateOnExit>,
            exit_code: Option<i32>,
            reason: Option<&'static str>,
            wanted: Option<&'static str>,
        }
        let rows = vec![
            Row {
                case: "an exit code the only condition does not name",
                conditions: vec![condition(Some("7"), None, None, "EXIT")],
                exit_code: Some(3),
                reason: Some(EXITED),
                wanted: None,
            },
            Row {
                case: "an exit code the only condition names",
                conditions: vec![condition(Some("7"), None, None, "EXIT")],
                exit_code: Some(7),
                reason: Some(EXITED),
                wanted: Some("EXIT"),
            },
            Row {
                case: "the first of two conditions decides",
                conditions: vec![
                    condition(Some("1*"), None, None, "retry"),
                    condition(Some("137"), None, None, "EXIT"),
                ],
                exit_code: Some(137),
                reason: Some(EXITED),
                wanted: Some("retry"),
            },
            Row {
                case: "two stated members, one of which does not match",
                conditions: vec![condition(Some("3"), None, Some("Host EC2*"), "EXIT")],
                exit_code: Some(3),
                reason: Some(EXITED),
                wanted: None,
            },
            Row {
                case: "two stated members, both of which match",
                conditions: vec![condition(Some("3"), None, Some("Essential*"), "EXIT")],
                exit_code: Some(3),
                reason: Some(EXITED),
                wanted: Some("EXIT"),
            },
            Row {
                case: "a condition on the reason of an attempt that produced no exit status",
                conditions: vec![condition(None, Some("CannotPull*"), None, "EXIT")],
                exit_code: None,
                reason: Some("CannotPullContainerError: no such image"),
                wanted: Some("EXIT"),
            },
            Row {
                case: "a condition on the exit code of an attempt that produced none",
                conditions: vec![condition(Some("1"), None, None, "EXIT")],
                exit_code: None,
                reason: Some("CannotPullContainerError: no such image"),
                wanted: None,
            },
            Row {
                case: "no conditions at all",
                conditions: Vec::new(),
                exit_code: Some(3),
                reason: Some(EXITED),
                wanted: None,
            },
        ];
        for row in rows {
            let decided = matched_action(&row.conditions, row.exit_code, row.reason);
            assert_eq!(
                decided, row.wanted,
                "case {}: expected {:?}, got {decided:?}",
                row.case, row.wanted
            );
        }
    }
}
