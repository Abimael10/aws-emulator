// SPDX-License-Identifier: CC0-1.0

//! What a SANDBOX and the commands submitted to it come to as the clock moves them: the
//! transitions a sandbox's own record is owed ([`StoredSandbox`]) and the one a command
//! execution of that sandbox is owed ([`StoredCommandExecution`]).
//!
//! Both are the same rule the rest of this store follows — a transition is due at the
//! instant it was owed whether or not anybody called, and the reader that finds it due
//! materialises it — stated here for the sandbox plane rather than beside the build plane's
//! own records.

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

    /// Materialise this command execution's terminal state once its due instant has
    /// passed.
    ///
    /// Idempotent and purely a function of the stored instants: an execution that has
    /// already ended, or whose span has not elapsed, is left alone. It ends in
    /// [`COMMAND_EXECUTION_STATUS_FAULT`] because nothing here ran the command — see
    /// [`COMMAND_NOT_EXECUTED_MESSAGE`] — and therefore reports neither an
    /// `exitCode` nor any `standardOutputContent`: no process exited and no command
    /// wrote anything.
    ///
    /// `startTime` and `endTime` are both the due instant: the attempt was made and
    /// given up there, so the record carries no run duration for work nothing
    /// performed.
    pub fn settle(&mut self, now_millis: i64) {
        if self.is_terminal() || now_millis < self.settle_due_millis {
            return;
        }
        let due = self.settle_due_millis;
        self.command_execution.status = Some(COMMAND_EXECUTION_STATUS_FAULT.to_string());
        self.command_execution.start_time = Some(datetime(due));
        self.command_execution.end_time = Some(datetime(due));
        self.command_execution.standard_err_content =
            Some(COMMAND_NOT_EXECUTED_MESSAGE.to_string());
    }
}
