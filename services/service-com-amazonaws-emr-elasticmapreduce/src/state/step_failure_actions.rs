// SPDX-License-Identifier: CC0-1.0

//! What a step's `ActionOnFailure` DOES once that step's own work has failed.
//!
//! # The rule this module implements
//!
//! "The action to take when the step fails. Use one of the following values:
//! `TERMINATE_CLUSTER` — Shuts down the cluster. `CANCEL_AND_WAIT` — Cancels any pending
//! steps and returns the cluster to the `WAITING` state. `CONTINUE` — Continues to the next
//! step in the queue. `TERMINATE_JOB_FLOW` — Shuts down the cluster" (`StepConfig$ActionOnFailure`
//! doc). So the member is not a label a read echoes back: it decides what becomes of the rest
//! of the queue and of the cluster the moment the step it belongs to fails.
//!
//! The same doc states when it decides NOTHING: "if you change a cluster's
//! `StepConcurrencyLevel` to be greater than 1 while a step is running, the `ActionOnFailure`
//! parameter may not behave as you expect. In this case, for a step that fails with this
//! parameter set to `CANCEL_AND_WAIT`, pending steps and the running step are not canceled;
//! for a step that fails with this parameter set to `TERMINATE_CLUSTER`, the cluster does not
//! terminate." A cluster running more than one step at a time therefore applies no action at
//! all — which is also why the two actions are REFUSED at submission on such a cluster
//! ([`step_action_on_failure_violation`]).
//!
//! # Which failure applies it
//!
//! Only a failure the step's own work ANSWERED ([`StepRecord::work_answered`]): a
//! `spark-submit` whose application exited non-zero, a statement the metastore refused, a
//! command that exited non-zero. The other failure a step of this service can end in is this
//! emulator's own — it ran the step lifecycle and nothing ran the submission
//! ([`STEP_NOT_EXECUTED_REASON`]) — and that is no answer about the submitted work, so it
//! cancels nothing and shuts nothing down. A step carrying no `ActionOnFailure` at all names
//! no action, so nothing is applied for it either.
//!
//! # Where it is applied
//!
//! In the cluster's own step runner ([`ClusterRecord::advance_to`]), from instants that are a
//! function of what is persisted: the pending steps of a stopped queue are cancelled as of the
//! failing step's `EndDateTime`, and the shutdown a terminating action brings is due one
//! [`TRANSITION_SPAN`] after that same instant — so `Cluster.Timeline.EndDateTime` is never
//! earlier than the terminating step's own, and two reads of one cluster report the same
//! timeline. Nothing sleeps.

use super::*;

/// The `ActionOnFailure` values that SHUT THE CLUSTER DOWN when the step carrying one fails:
/// "`TERMINATE_CLUSTER` — Shuts down the cluster" and "`TERMINATE_JOB_FLOW` — Shuts down the
/// cluster. `TERMINATE_JOB_FLOW` is provided for backward compatibility"
/// (`StepConfig$ActionOnFailure` doc).
pub const CLUSTER_SHUTDOWN_FAILURE_ACTIONS: &[&str] = &["TERMINATE_CLUSTER", "TERMINATE_JOB_FLOW"];

/// The `ActionOnFailure` value that stops the queue and leaves the cluster up: "`CANCEL_AND_WAIT`
/// — Cancels any pending steps and returns the cluster to the `WAITING` state"
/// (`StepConfig$ActionOnFailure` doc).
pub const CANCEL_AND_WAIT_FAILURE_ACTION: &str = "CANCEL_AND_WAIT";

/// `ClusterStatus$StateChangeReason.Message` for the shutdown a failed step's
/// `TERMINATE_CLUSTER`/`TERMINATE_JOB_FLOW` brings, beside the modelled code for it
/// (`STEP_FAILURE`).
///
/// The sentence says WHICH shutdown this is — the step that failed asked for it — in the same
/// fixed form as the other shutdown messages this service answers
/// ([`CLUSTER_STEPS_COMPLETED_REASON`]): nothing of the request or of the cluster is
/// interpolated into it.
pub const CLUSTER_STEP_FAILURE_REASON: &str = "Shut down as step failed";

/// How `AddJobFlowSteps` refuses a step whose `ActionOnFailure` names an action its cluster's
/// `StepConcurrencyLevel` does not admit.
///
/// "If a cluster's `StepConcurrencyLevel` is greater than 1, do not use `AddJobFlowSteps` to
/// submit a step with this parameter set to `CANCEL_AND_WAIT` or `TERMINATE_CLUSTER`. The step
/// is not submitted and the action fails with a message that the `ActionOnFailure` setting is
/// not valid." (`StepConfig$ActionOnFailure` doc.) Recorded behavior for the wording: the
/// sentence names the ACTION the request asked for — its one variable part — and nothing else
/// of the request or of the cluster.
#[must_use]
pub fn invalid_action_on_failure_message(action: &str) -> String {
    format!("Action on failure '{action}' is invalid.")
}

/// Whether `action` is one a cluster running `step_concurrency_level` steps at once refuses at
/// submission, and the sentence it is refused with.
///
/// The two actions that act on the QUEUE are the ones the documented refusal names, and the
/// level it names is "greater than 1" — so a cluster at the default level of one admits every
/// value the model declares. A step naming no action at all names nothing to refuse.
#[must_use]
pub fn step_action_on_failure_violation(
    action: Option<&str>,
    step_concurrency_level: i32,
) -> Option<String> {
    if step_concurrency_level <= 1 {
        return None;
    }
    let action = action?;
    let refused = action == CANCEL_AND_WAIT_FAILURE_ACTION
        // The doc names `CANCEL_AND_WAIT` and `TERMINATE_CLUSTER`; `TERMINATE_JOB_FLOW` is
        // the same shutdown under its backward-compatible name, so it is refused with them.
        || CLUSTER_SHUTDOWN_FAILURE_ACTIONS.contains(&action);
    refused.then(|| invalid_action_on_failure_message(action))
}

impl StepRecord {
    /// Whether this step's end is one its own WORK answered, as against the end a step whose
    /// submission nothing ran reaches.
    ///
    /// Every outcome this service records is an answer about the submitted work except ONE,
    /// the shared spelling of "nothing ran it" ([`not_executed`], whose reason is
    /// [`STEP_NOT_EXECUTED_REASON`]). The distinction is what keeps this emulator's own
    /// non-execution from cancelling a queue or shutting a cluster down: that is not the
    /// application failing, so it decides nothing about what follows the step.
    #[must_use]
    pub fn work_answered(&self) -> bool {
        self.outcome.as_ref().is_some_and(|outcome| {
            outcome
                .failure
                .as_ref()
                .is_none_or(|failure| failure.reason != STEP_NOT_EXECUTED_REASON)
        })
    }

    /// The `ActionOnFailure` this step's FAILURE calls for, or `None` when it calls for none:
    /// the step has not failed, its failure is this emulator's own rather than its work's
    /// ([`Self::work_answered`]), or it carries no action.
    pub(super) fn answered_failure_action(&self) -> Option<&str> {
        if self.state != step_state::FAILED || !self.work_answered() {
            return None;
        }
        self.action_on_failure.as_deref()
    }

    /// Whether this step's failure STOPS the queue behind it: both the cancelling action and
    /// the two shutting ones cancel the steps that have not run — a cluster that is going away
    /// runs none of them either.
    pub(super) fn stops_the_queue(&self) -> bool {
        self.answered_failure_action().is_some_and(|action| {
            action == CANCEL_AND_WAIT_FAILURE_ACTION
                || CLUSTER_SHUTDOWN_FAILURE_ACTIONS.contains(&action)
        })
    }

    /// Cancel this step because the queue it was waiting in was STOPPED at `stopped_at`.
    ///
    /// Such a step never ran, so `CANCELLED` is the state `StepState` carries for it — the
    /// same state a step that never started reaches when its cluster goes away
    /// ([`Self::stop_with_cluster`]) — and it ends at the instant the queue stopped rather
    /// than when somebody read it. A step already owed an earlier stop keeps that one, so a
    /// cancel the caller asked for is never pushed out, and a step that has already ended
    /// keeps what it has.
    pub(super) fn cancel_with_queue(&mut self, stopped_at: SystemTime) {
        if self.is_terminal() {
            return;
        }
        self.stop_due_at = Some(
            self.stop_due_at
                .map_or(stopped_at, |owed| owed.min(stopped_at)),
        );
        self.stop_state = Some(step_state::CANCELLED.to_string());
    }
}

impl ClusterRecord {
    /// Whether this cluster applies a failed step's `ActionOnFailure` at all.
    ///
    /// A cluster running more than one step at a time does not: "for a step that fails with
    /// this parameter set to `CANCEL_AND_WAIT`, pending steps and the running step are not
    /// canceled; for a step that fails with this parameter set to `TERMINATE_CLUSTER`, the
    /// cluster does not terminate" (`StepConfig$ActionOnFailure` doc).
    pub(super) fn applies_step_failure_actions(&self) -> bool {
        self.step_concurrency_level <= 1
    }

    /// Start the shutdown a failed step's `TERMINATE_CLUSTER`/`TERMINATE_JOB_FLOW` brings,
    /// answering whether it was started.
    ///
    /// A step failure is a failure of the CLUSTER's work, so the cluster shuts down carrying
    /// the modelled reason for it (`ClusterStateChangeReasonCode` `STEP_FAILURE`) and ends
    /// `TERMINATED_WITH_ERRORS` ([`terminal_cluster_state`]) rather than `TERMINATED`. The
    /// shutdown is owed one [`TRANSITION_SPAN`] after the step's own end, so the cluster's
    /// `Timeline.EndDateTime` is never earlier than that step's.
    ///
    /// Answers `false` without touching the cluster when no step of it asked for the shutdown,
    /// when it is already shutting down or gone, when its concurrency level means the action
    /// does nothing ([`Self::applies_step_failure_actions`]), and when the caller holds it
    /// under termination protection — which "prevents the Amazon EC2 instances from being
    /// terminated" (`Cluster$TerminationProtected` doc), so a cluster launched under it keeps
    /// the nodes a caller asked to keep.
    pub(super) fn begin_step_failure_shutdown(&mut self, now: SystemTime) -> bool {
        if self.is_terminal()
            || self.status.state == cluster_state::TERMINATING
            || self.termination_protection_holds()
            || !self.applies_step_failure_actions()
        {
            return false;
        }
        let Some(failed_at) = self
            .steps
            .iter()
            .find(|step| {
                step.answered_failure_action()
                    .is_some_and(|action| CLUSTER_SHUTDOWN_FAILURE_ACTIONS.contains(&action))
            })
            .and_then(|step| step.end_date_time)
        else {
            return false;
        };
        self.status.state = cluster_state::TERMINATING.to_string();
        self.status.state_change_reason = Some(StateChangeReasonRecord {
            code: cluster_state_change_reason_code::STEP_FAILURE.to_string(),
            message: Some(CLUSTER_STEP_FAILURE_REASON.to_string()),
        });
        self.status.bootstrapping_due_at = None;
        self.status.ready_due_at = None;
        let terminated_at = failed_at + TRANSITION_SPAN;
        self.status.terminated_due_at = Some(terminated_at);
        // The cluster takes its step queue with it, exactly as a caller's own shutdown and a
        // bootstrap failure do: no step stays `PENDING` for a cluster whose nodes are going
        // away. A step the stopped queue already cancelled keeps the earlier instant.
        for step in &mut self.steps {
            step.stop_with_cluster(terminated_at);
            step.advance_to(now);
        }
        true
    }
}
