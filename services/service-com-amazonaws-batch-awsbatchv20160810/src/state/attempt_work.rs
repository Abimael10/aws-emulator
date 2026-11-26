// SPDX-License-Identifier: CC0-1.0

//! How long the WORK of one job attempt takes, and what that makes of its `timeout`.
//!
//! # The rule this module implements
//!
//! `JobTimeout$attemptDurationSeconds` is "the job timeout time (in seconds) that's measured
//! from the job attempt's `startedAt` timestamp. After this time passes, Batch terminates your
//! jobs if they aren't finished" — so what the timeout asks about an attempt is whether its
//! WORK FINISHED inside the duration the request allowed it. An attempt that did not is ended
//! by the SERVICE: the job reaches the terminal `FAILED` with the reason of that failure class
//! and no exit code at all, because the container it was running never exited
//! ([`super::attempt_timeout`] owns that end, and this module owns the question).
//!
//! # How long the work takes HERE
//!
//! A container's program is performed in compressed time: the seconds a program's `sleep`
//! names are its own account of how long it took and no clock of this emulator is spent on
//! them (`super::container_run`, and the Amazon ECS crate's container session for a job whose
//! container runs as a task — the suite
//! `job_settles_whatever_wait_its_container_states.rs` pins that reading). So the wall clock
//! between an attempt's `startedAt` and the instant its program ended says nothing about how
//! long the work the request declared takes: the answer is the duration that program STATES.
//!
//! That duration is read here, once per job, from the job's own effective container program
//! ([`stated_work_millis`]) — and only for a job that states an
//! `attemptDurationSeconds` at all, because nothing else in this service asks the question.
//! The reading PERFORMS no work of the job: it keeps no output, takes no exit status, opens no
//! log stream and is reported nowhere; what the job reports of its run is the one performance
//! of it, wherever that happens. It is CPU-bound, so it is taken off the request worker with
//! this store's lock released, exactly as performing a container's program is
//! ([`settle_attempt_work_accounts`]).
//!
//! The comparison is then pure: an attempt whose work states more than its
//! `attemptDurationSeconds` allows did not finish in time, whatever instant anybody looked at
//! it ([`ComAmazonawsBatchAwsbatchv20160810State::attempt_outlasted_its_timeout`]), and an
//! attempt inside its allowance is left to whatever its work comes to. A job that states no
//! timeout is never asked about at all.

use super::*;

/// The longest wait ONE job's program may state, in seconds.
///
/// THIS NUMBER IS THE EMULATOR'S: the words a program states its waits in are a caller's, so
/// the duration they can add up to is bounded here rather than by the request. A day is far
/// above the longest `attemptDurationSeconds` a job carries in practice, so the comparison
/// above is decided by the request's own allowance and not by this ceiling.
pub const MAX_STATED_WORK_SECONDS: u64 = 86_400;

/// How many programs ONE drain reads the stated duration of.
///
/// The same bound, and for the same reason, as the number of container programs one drain
/// performs ([`MAX_CONTAINER_RUNS_PER_DRAIN`]): reading a program's own account of its
/// duration is CPU-bound work, and the jobs waiting for one are bounded only by the jobs store
/// itself. What a drain leaves behind is read a tick later.
pub const MAX_WORK_ACCOUNTS_PER_DRAIN: usize = 10;

/// The duration `program` STATES it takes, in milliseconds.
///
/// The waits the program names, as the one session this workspace performs a caller-declared
/// program in accounts for them (`::service_common::posix_shell_session`, whose `sleep`
/// "states the TIME the program took"), bounded by [`MAX_STATED_WORK_SECONDS`]. Nothing of
/// this pass is reported: no output is kept, the status it ended with is discarded, and the
/// session's tree goes with it.
///
/// Pure computation over the caller's own words: no process is started, no host file is
/// opened and no socket is reached.
#[must_use]
pub fn stated_work_millis(program: &[String], environment: &[(String, String)]) -> i64 {
    let Some(name) = performable_program(program) else {
        // A program nothing performs states no duration: there is no work for any seconds to
        // be the account of.
        return 0;
    };
    let mut words: Vec<&str> = vec![name];
    words.extend(program.iter().skip(1).map(String::as_str));
    let line = ::shell_words::join(words);
    let variables: BTreeMap<String, String> = environment.iter().cloned().collect();
    let bounds = ::service_common::posix_shell_session::Bounds {
        sleep_seconds: MAX_STATED_WORK_SECONDS,
        ..::service_common::posix_shell_session::Bounds::default()
    };
    let config = ::service_common::posix_shell_session::SessionConfig {
        bounds,
        // No output is kept: this pass reports nothing the program wrote.
        output: None,
        ..::service_common::posix_shell_session::SessionConfig::default()
    };
    let mut session =
        ::service_common::posix_shell_session::ShellSession::new(variables, config)
            .with_file_utilities();
    let _ = session.run(&line);
    i64::try_from(session.slept().as_millis()).unwrap_or(i64::MAX)
}

/// [`stated_work_millis`] off the request worker, the way a container's program is performed
/// off it ([`run_container_program_off_worker`]).
///
/// `None` is a fault of this process rather than of the request: nothing is recorded for it
/// and the next drain reads it again.
pub async fn stated_work_millis_off_worker(
    program: &[String],
    environment: &[(String, String)],
) -> Option<i64> {
    let program = program.to_vec();
    let environment = environment.to_vec();
    ::tokio::task::spawn_blocking(move || stated_work_millis(&program, &environment))
        .await
        .ok()
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The `attemptDurationSeconds` `job` allows one attempt, when it states one at all.
    ///
    /// The ONE reading of that member for both questions this service asks about it — the
    /// deadline on a running attempt ([`super::attempt_timeout`]) and the allowance the work
    /// below is held to — so the two cannot disagree. A non-positive value states no
    /// allowance.
    pub(super) fn attempt_allowance_millis(job: &Job) -> Option<i64> {
        let seconds = job.timeout.as_ref()?.attempt_duration_seconds?;
        (seconds > 0).then(|| i64::from(seconds).saturating_mul(1_000))
    }

    /// Whether `job` is one whose work this service still has to read the duration of: a live
    /// job that states an attempt allowance and has not been read yet.
    ///
    /// Asked of the record alone, so the list of such jobs and the question the clock pump
    /// asks cannot disagree and asking costs nothing but a read.
    fn awaits_a_work_account(job: &Job) -> bool {
        !is_terminal_status(&job.status)
            && job.work_stated_millis.is_none()
            && Self::attempt_allowance_millis(job).is_some()
    }

    /// The jobs whose work this service still has to read the duration of, with the program
    /// each one declares.
    ///
    /// Read from the job's own effective container, with this job's parameters substituted in
    /// ([`super::job_parameters`]) — the same container the run itself is performed from, so
    /// the duration read here is the duration of the program the job really runs. Costs one
    /// walk of the jobs map, which [`limits::MAX_JOBS`] bounds, and answers at most
    /// [`MAX_WORK_ACCOUNTS_PER_DRAIN`] of them.
    pub fn jobs_awaiting_work_account(&self) -> Vec<JobContainerRun> {
        self.jobs
            .values()
            .filter(|job| Self::awaits_a_work_account(job))
            .filter_map(|job| self.job_container_run_request(&job.job_id))
            .take(MAX_WORK_ACCOUNTS_PER_DRAIN)
            .collect()
    }

    /// Record how long each job's work STATES it takes.
    ///
    /// Written once per job: a job that already carries a reading keeps it, so what a reader
    /// sees cannot change under it.
    pub fn record_work_accounts(&mut self, accounts: Vec<(String, i64)>) {
        for (id, stated) in accounts {
            if let Some(job) = self.jobs.get_mut(&id) {
                if job.work_stated_millis.is_none() {
                    job.work_stated_millis = Some(stated);
                }
            }
        }
    }

    /// Whether any job is still waiting for that reading, which is what keeps the clock pump
    /// awake until every job can take its next move.
    pub(super) fn has_pending_work_accounts(&self) -> bool {
        self.jobs.values().any(Self::awaits_a_work_account)
    }

    /// The instant `id`'s attempt ran out of the time its `timeout` allowed it, when the work
    /// that attempt is running STATES more than that allowance — and `None` for every attempt
    /// whose work fits inside it.
    ///
    /// `None` too while the duration of the work has not been read yet, for a job that states
    /// no allowance, and for one whose attempt has not STARTED or has already ended: the
    /// member is "measured from the job attempt's `startedAt` timestamp", which is what
    /// [`Self::attempt_deadline`] answers.
    pub(super) fn attempt_outlasted_its_timeout(&self, id: &str) -> Option<i64> {
        let job = self.jobs.get(id)?;
        let stated = job.work_stated_millis?;
        let allowance = Self::attempt_allowance_millis(job)?;
        if stated <= allowance {
            return None;
        }
        self.attempt_deadline(id)
    }
}

/// Read the duration the work of every job waiting for one states, and record it.
///
/// Called by the drain every operation of this service's job plane makes, and by the clock
/// pump, with this store's guard dropped: reading a caller's program is CPU-bound work, so no
/// lock of this store is held while it happens. Nothing is read twice — a job that already
/// carries a reading is not collected.
pub async fn settle_attempt_work_accounts<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The guard is taken and dropped inside this statement: nothing of this store is held
    // while the programs below are read.
    let waiting = state.lock().jobs_awaiting_work_account();
    if waiting.is_empty() {
        return;
    }
    let mut accounts: Vec<(String, i64)> = Vec::with_capacity(waiting.len());
    for job in waiting {
        if let Some(stated) = stated_work_millis_off_worker(&job.program, &job.environment).await {
            accounts.push((job.job_id, stated));
        }
    }
    state.lock().record_work_accounts(accounts);
}
