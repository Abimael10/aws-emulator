// SPDX-License-Identifier: CC0-1.0

//! The ASYNCHRONOUS half of a code interpreter session's command plane: the tasks
//! `startCommandExecution` submits, `getTask` reports and `stopTask` cancels.
//!
//! `startCommandExecution` answers at once with a task id and a non-terminal status, so the
//! command it names is work the session OWES rather than work it has already done. That is
//! what this module holds: one record per submitted task, carrying the command line, the
//! instant its run falls due, and — once it has been performed — the two streams and the
//! status it ended with.
//!
//! The command is performed when it falls DUE, by the next call this session serves
//! ([`CommandTaskRecord::due_at`]): there is nothing else in this emulator to run it, so a
//! task converges the way every other clock-owed transition here does, materialised by the
//! first caller to look after the instant it was owed. Its effect on the workspace lands with
//! it, so a file the task's command wrote is one the session's file tools read from that
//! instant on. A `stopTask` before that instant CANCELS it: the command is never performed,
//! which is what cancelling a submitted task means.
//!
//! `TaskStatus`'s values are the whole lifecycle (source: smithy_query query="shape"
//! shape="TaskStatus", values submitted | working | completed | canceled | failed) and
//! [`CommandTaskRecord::status_as_of`] is the one place a task's own state and the clock are
//! read into one of them.

use std::time::{Duration, SystemTime};

use super::mint_alphanumeric_session_id;

/// `TaskStatus` for a task whose command has not been started yet.
pub const TASK_STATUS_SUBMITTED: &str = "submitted";
/// `TaskStatus` for a task whose command is being performed.
pub const TASK_STATUS_WORKING: &str = "working";
/// `TaskStatus` for a task whose command ran to success.
pub const TASK_STATUS_COMPLETED: &str = "completed";
/// `TaskStatus` for a task a `stopTask` call cancelled before its command ran.
pub const TASK_STATUS_CANCELED: &str = "canceled";
/// `TaskStatus` for a task whose command ran and ended with a non-zero status.
pub const TASK_STATUS_FAILED: &str = "failed";

/// How long after it is submitted a task's command falls due.
///
/// An EMULATOR span, not a documented AWS duration: AWS starts the command on a worker of
/// its own and the time it takes is the command's. What this span states is that a submitted
/// task is NOT already finished — a caller that polls straight after submitting one sees the
/// non-terminal status AWS answers with, and a `stopTask` sent then has something to cancel
/// — while still converging inside one interaction.
pub const COMMAND_TASK_SPAN: Duration = Duration::from_secs(3);

/// How long after it is submitted a task is reported as `working` rather than `submitted`:
/// the first half of [`COMMAND_TASK_SPAN`], so both non-terminal statuses of the lifecycle
/// are observable.
const COMMAND_TASK_PICKUP: Duration = Duration::from_secs(1);

/// The most tasks one session's record keeps. An EMULATOR ceiling: a caller chooses how many
/// tasks it submits and how long each command line is, so a submission past it is refused
/// once the tasks that have reached a terminal status have been pruned
/// (`CodeInterpreterSessionRecord::room_for_a_task`), and the oldest task gives way if a
/// record ever passes it anyway.
pub const MAX_SESSION_TASKS: usize = 64;

/// The most bytes of command line one task or one synchronous command carries.
///
/// An EMULATOR ceiling, not a modelled one: `ToolArguments$command` targets `MaxLenString`,
/// which admits 100 MB (source: smithy_query query="shape" shape="ToolArguments"), and a
/// submitted task HOLDS its command until it is performed — so this is what keeps what one
/// session stores proportional to the calls it served rather than to a caller-chosen size.
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;

/// The most tasks ONE call performs when it finds several of them due.
///
/// An EMULATOR ceiling, for the same reason: how many tasks are due at one instant follows
/// from how many an earlier caller submitted, and the work of one tool call stays
/// proportional to that call — the tasks past it are performed by the calls after it, which
/// is what their own due instants already allow for.
pub const MAX_DUE_TASKS_PER_CALL: usize = 4;

/// What performing one task's command came to.
#[derive(Debug, Clone)]
pub struct CommandTaskOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// How long the shell spent on the command, reported as the result's `executionTime`.
    pub elapsed: Duration,
    /// When the command was performed.
    pub performed_at: SystemTime,
    /// When this task's work is over: [`Self::performed_at`] plus the time the command
    /// itself stated it took (`CommandRun::slept`). A task whose command waits is `working`
    /// until then, which is what makes it take the time its own program asked for.
    pub finishes_at: SystemTime,
}

/// One task of a code interpreter session: the command it was submitted with and everything
/// a later `getTask` reports about it.
#[derive(Debug, Clone)]
pub struct CommandTaskRecord {
    /// Minted by [`mint_command_task_id`]; never client-supplied.
    pub task_id: String,
    /// The command line this task performs, as the submitting call named it.
    pub command: String,
    pub submitted_at: SystemTime,
    /// When this task's command falls due, which is [`COMMAND_TASK_SPAN`] after it was
    /// submitted.
    pub due_at: SystemTime,
    /// When a `stopTask` call cancelled this task, or `None` while none has.
    pub canceled_at: Option<SystemTime>,
    /// What performing the command came to, or `None` while it has not been performed.
    pub outcome: Option<CommandTaskOutcome>,
    /// Set while a call is performing this task's command, so two calls that look at the
    /// same due task do not both run it.
    pub performing: bool,
    /// The value [`super::ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState::next_seq`]
    /// handed this task: its eviction order inside the session.
    pub seq: u64,
}

impl CommandTaskRecord {
    /// A task as the `startCommandExecution` call that submits it leaves it: nothing
    /// performed, and its command due [`COMMAND_TASK_SPAN`] from `now`.
    #[must_use]
    pub fn submitted(seq: u64, command: String, now: SystemTime) -> Self {
        Self {
            task_id: mint_command_task_id(seq),
            command,
            submitted_at: now,
            due_at: now.checked_add(COMMAND_TASK_SPAN).unwrap_or(now),
            canceled_at: None,
            outcome: None,
            performing: false,
            seq,
        }
    }

    /// This task's `TaskStatus` at `now`, read off its own state and the clock.
    #[must_use]
    pub fn status_as_of(&self, now: SystemTime) -> &'static str {
        if self.canceled_at.is_some() {
            return TASK_STATUS_CANCELED;
        }
        match &self.outcome {
            // A command that has been performed but whose own program asked to take longer
            // is still working: what it waited for has not passed yet.
            Some(outcome) if now < outcome.finishes_at => TASK_STATUS_WORKING,
            Some(outcome) if outcome.exit_code == 0 => TASK_STATUS_COMPLETED,
            Some(_) => TASK_STATUS_FAILED,
            None => {
                let picked_up = self
                    .submitted_at
                    .checked_add(COMMAND_TASK_PICKUP)
                    .unwrap_or(self.submitted_at);
                if self.performing || now >= picked_up {
                    TASK_STATUS_WORKING
                } else {
                    TASK_STATUS_SUBMITTED
                }
            }
        }
    }

    /// Whether this task's command falls due at `now` and nothing is performing it yet:
    /// what the call that materialises it looks for.
    #[must_use]
    pub fn due_at_instant(&self, now: SystemTime) -> bool {
        self.canceled_at.is_none()
            && self.outcome.is_none()
            && !self.performing
            && now >= self.due_at
    }

    /// Whether this task has reached a status nothing moves it out of.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.canceled_at.is_some() || self.outcome.is_some()
    }

    /// What this task reports as the time its command took: the shell's own time on it and
    /// the time the command stated it waited, which is the span a caller observed.
    #[must_use]
    pub fn reported_duration(&self) -> Option<Duration> {
        let outcome = self.outcome.as_ref()?;
        let waited = outcome
            .finishes_at
            .duration_since(outcome.performed_at)
            .unwrap_or(Duration::ZERO);
        Some(outcome.elapsed.saturating_add(waited))
    }

    /// Cancel this task at `now`, as `stopTask` does, answering whether there was anything
    /// to cancel: a task that has reached a status nothing moves it out of — completed,
    /// failed or already cancelled — has not.
    pub fn cancel(&mut self, now: SystemTime) -> bool {
        if matches!(
            self.status_as_of(now),
            TASK_STATUS_COMPLETED | TASK_STATUS_FAILED | TASK_STATUS_CANCELED
        ) {
            return false;
        }
        self.canceled_at = Some(now);
        true
    }
}

/// Mint a task id.
///
/// `ToolResultStructuredContent$taskId` targets a plain `String` with no pattern (source:
/// smithy_query query="shape" shape="ToolResultStructuredContent"), so the FORM is this
/// emulator's: the `task-` prefix over the same unique-per-call body every id of this
/// service's tools planes is built from, which keeps two tasks submitted in one millisecond
/// apart.
#[must_use]
pub fn mint_command_task_id(seq: u64) -> String {
    format!("task-{}", mint_alphanumeric_session_id(seq))
}
