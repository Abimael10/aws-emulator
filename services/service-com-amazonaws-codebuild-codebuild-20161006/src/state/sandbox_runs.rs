// SPDX-License-Identifier: CC0-1.0

//! What a SANDBOX and the commands submitted to it come to as the clock moves them: the
//! transitions a sandbox's own record is owed ([`StoredSandbox`]) and what one command
//! execution of that sandbox records once its session has run it
//! ([`StoredCommandExecution`]).
//!
//! Both are the same rule the rest of this store follows — a transition is due at the
//! instant it was owed whether or not anybody called, and the reader that finds it due
//! materialises it — stated here for the sandbox plane rather than beside the build plane's
//! own records. What a command execution's own transition WAITS for is the sandbox's session
//! getting round to it, which
//! [`ComAmazonawsCodebuildCodebuild20161006State::run_due_command_executions`] drives.

use super::*;

impl StoredSandbox {
    /// Whether this sandbox has reached a status it can never leave on its own.
    ///
    /// Read from the instant the record carries rather than from the status string:
    /// `endTime` is set exactly when the sandbox stopped, so nothing has to keep a
    /// list of terminal status names in step with it.
    pub fn is_terminal(&self) -> bool {
        self.sandbox.end_time.is_some()
    }

    /// Whether this sandbox's session is LIVE — provisioned, not ended — which is what
    /// a command execution or a connection needs to act on.
    ///
    /// Read off the record's own status, so it is true exactly for the sandbox
    /// [`Self::settle`] has moved to [`SANDBOX_STATUS_RUNNING`]: one still in the queue
    /// has no session yet, and one that stopped or timed out no longer has one. Call it
    /// through [`ComAmazonawsCodebuildCodebuild20161006State::settled_sandbox`], so the
    /// transitions the clock owes the sandbox are materialised before it is judged.
    pub fn is_running(&self) -> bool {
        !self.is_terminal() && self.sandbox.status.as_deref() == Some(SANDBOX_STATUS_RUNNING)
    }

    /// The minutes this sandbox waits before timing out, from the modelled member it
    /// carries: `Sandbox$timeoutInMinutes` is "How long, in minutes, from 5 to 2160 (36
    /// hours), for CodeBuild to wait before timing out this sandbox if it does not get
    /// marked as completed". A record that carries none falls back to the build default
    /// AWS documents for a project, [`DEFAULT_BUILD_TIMEOUT_MINUTES`], which is the
    /// value `CreateProject` stores when the caller omits one.
    pub fn timeout_minutes(&self) -> i32 {
        self.sandbox
            .timeout_in_minutes
            .unwrap_or(DEFAULT_BUILD_TIMEOUT_MINUTES)
    }

    /// How long this sandbox's session stays live before the clock reaps it: the wait
    /// its [`Self::timeout_minutes`] documents, on the compressed clock
    /// [`SANDBOX_IDLE_CLOCK_SCALE`] describes and inside the bounds it is clamped to.
    ///
    /// Monotonic in the documented wait, so a sandbox configured to wait longer is
    /// never reaped sooner than one configured to wait less.
    pub fn idle_span_millis(&self) -> i64 {
        let documented = i64::from(self.timeout_minutes()).saturating_mul(MILLIS_PER_MINUTE);
        (documented / SANDBOX_IDLE_CLOCK_SCALE)
            .clamp(MIN_SANDBOX_IDLE_SPAN_MILLIS, MAX_SANDBOX_IDLE_SPAN_MILLIS)
    }

    /// Epoch millis at which a sandbox nobody stopped is reaped by the clock: its
    /// [`Self::idle_span_millis`] measured from the instant the session went live, since
    /// the wait AWS documents runs "before timing out this sandbox if it does not get
    /// marked as completed".
    pub fn timeout_due_millis(&self) -> i64 {
        self.settle_due_millis
            .saturating_add(self.idle_span_millis())
    }

    /// Materialise the transitions the clock already owes this sandbox.
    ///
    /// Idempotent and purely a function of the stored instants, so repeating it (or
    /// skipping a read entirely) changes nothing:
    ///
    /// * before `settle_due_millis` the sandbox is still `QUEUED`;
    /// * from `settle_due_millis` it has been provisioned and its session is live —
    ///   [`SANDBOX_STATUS_RUNNING`], sitting in the [`SANDBOX_PHASE_RUN_SANDBOX`]
    ///   phase, which is where it STAYS, because a sandbox is a live session rather
    ///   than a job with an outcome;
    /// * from [`Self::timeout_due_millis`] it has sat out its own wait without being
    ///   marked completed, so CodeBuild reaps it — which ends it in exactly the status
    ///   an explicit `StopSandbox` would have, [`SANDBOX_STATUS_STOPPED`].
    ///
    /// A sandbox a caller already stopped is terminal and is left alone.
    pub fn settle(&mut self, now_millis: i64) {
        if self.is_terminal() || now_millis < self.settle_due_millis {
            return;
        }
        self.run();
        let reaped_at = self.timeout_due_millis();
        if now_millis >= reaped_at {
            self.end(SANDBOX_STATUS_STOPPED, reaped_at);
        }
    }

    /// Epoch millis this sandbox's session ENDED at — `None` while it is still live.
    ///
    /// Read off the instant the record carries, the way [`Self::is_terminal`] is: past it
    /// the sandbox has no session, so nothing of it can run a command.
    pub fn ended_millis(&self) -> Option<i64> {
        self.sandbox
            .end_time
            .as_ref()
            .and_then(|at| at.to_millis().ok())
    }

    /// Move this sandbox to the live state its provisioning span ended in.
    fn run(&mut self) {
        let requested = self.request_millis;
        let started = self.settle_due_millis;
        self.sandbox.status = Some(SANDBOX_STATUS_RUNNING.to_string());
        self.sandbox.start_time = Some(datetime(started));
        if let Some(session) = self.sandbox.current_session.as_mut() {
            session.status = Some(SANDBOX_STATUS_RUNNING.to_string());
            session.current_phase = Some(SANDBOX_PHASE_RUN_SANDBOX.to_string());
            session.end_time = None;
            session.phases = Some(running_session_phases(requested, started));
        }
    }

    /// End this sandbox at `ended_millis` with the terminal `status` the session's
    /// open phase took — [`SANDBOX_STATUS_STOPPED`], whether the clock reaped it or a
    /// `StopSandbox` call ended it. The phase that takes the status is whichever one the
    /// sandbox was sitting in: the queue if it never got provisioned, the live session
    /// if it did.
    pub fn end(&mut self, status: &str, ended_millis: i64) {
        if self.is_terminal() {
            return;
        }
        let requested = self.request_millis;
        let started = self.settle_due_millis;
        let was_running = ended_millis >= started;
        self.sandbox.status = Some(status.to_string());
        self.sandbox.end_time = Some(datetime(ended_millis));
        if let Some(session) = self.sandbox.current_session.as_mut() {
            session.status = Some(status.to_string());
            session.current_phase = Some(PHASE_COMPLETED.to_string());
            session.end_time = Some(datetime(ended_millis));
            session.phases = Some(if was_running {
                ended_session_phases(requested, started, ended_millis, status)
            } else {
                left_the_queue_session_phases(requested, ended_millis, status)
            });
        }
    }
}

impl StoredCommandExecution {
    /// Whether this command execution has reached a status it can never leave.
    ///
    /// Read from the instant the record carries rather than from the status string,
    /// the way [`StoredSandbox::is_terminal`] is: `endTime` is set exactly when the
    /// execution stopped, so nothing has to keep a list of terminal status names in
    /// step with it.
    pub fn is_terminal(&self) -> bool {
        self.command_execution.end_time.is_some()
    }

    /// Record what running this command in the sandbox's session answered
    /// ([`super::sandbox_shell::SandboxShell::run`]): the status it exited with — which is
    /// `SUCCEEDED` for zero and `FAILED` for anything else — the text it wrote on each
    /// stream, and the window it occupied.
    ///
    /// Idempotent in the sense every other settle here is: an execution that has already
    /// ended is left alone, so a command is run once however many readers arrive.
    pub fn record_run(&mut self, outcome: &super::sandbox_shell::CommandOutcome) {
        if self.is_terminal() {
            return;
        }
        self.command_execution.status = Some(
            if outcome.exit_code == 0 {
                COMMAND_EXECUTION_STATUS_SUCCEEDED
            } else {
                COMMAND_EXECUTION_STATUS_FAILED
            }
            .to_string(),
        );
        self.command_execution.exit_code = Some(outcome.exit_code.to_string());
        self.command_execution.standard_output_content = outcome.standard_output.clone();
        self.command_execution.standard_err_content = outcome.standard_err.clone();
        self.command_execution.start_time = Some(datetime(outcome.started_millis));
        self.command_execution.end_time = Some(datetime(outcome.ended_millis));
    }

    /// End this execution at `ended_millis` because the sandbox's session ended before
    /// anything could run it: a command can only run in a session, and the sandbox has none
    /// — stopped by the caller, or reaped by the clock.
    ///
    /// It therefore reports neither an `exitCode` nor any output: no program exited and
    /// nothing wrote anything. [`SANDBOX_STATUS_STOPPED`] is the status the sandbox's own
    /// session took, which is what ended this command with it.
    pub fn end_unrun(&mut self, ended_millis: i64) {
        if self.is_terminal() {
            return;
        }
        self.command_execution.status = Some(SANDBOX_STATUS_STOPPED.to_string());
        self.command_execution.start_time = Some(datetime(ended_millis));
        self.command_execution.end_time = Some(datetime(ended_millis));
    }
}
