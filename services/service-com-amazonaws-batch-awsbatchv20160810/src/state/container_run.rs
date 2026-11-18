// SPDX-License-Identifier: CC0-1.0

//! The PROGRAM a placed job's container runs, and what the job reports from it.
//!
//! # The rule this module implements
//!
//! A job that has been placed runs the program its container declares:
//! `ContainerProperties$command` is "the command that's passed to the container", which is
//! the argument vector the image runs. So what the job does from `STARTING` on is what that
//! program does: `JobStatus$RUNNING` is where "the job is in the `RUNNING` state"  — the
//! container of the job has started — `ContainerDetail$exitCode` is "the exit code returned
//! upon completion", and the job's terminal status is the one that exit code decides:
//! `SUCCEEDED` for a container that exited zero, `FAILED` for any other status, each
//! carrying the `statusReason` the compute plane words for a task whose essential container
//! exited ([`ESSENTIAL_CONTAINER_EXITED_REASON`]). Nothing about the run is authored here:
//! the status, the output and the instants come from performing the caller's own program.
//!
//! # How the program is performed — and what \"performed\" excludes
//!
//! IN THIS PROCESS, and no process is started for it: there is no `std::process::Command`
//! here, no host `PATH` resolution, no host file opened or written, no host environment
//! variable read and no socket. The one session this workspace holds for a caller-declared
//! program does the work ([`::service_common::posix_shell_session`], which
//! `service-com-amazonaws-sagemaker-sagemaker`'s own container runs and
//! `service-com-amazonaws-codebuild-codebuild-20161006`'s buildspec lines drive too): the
//! GRAMMAR is a parser crate's — it builds a syntax tree and runs nothing — and what answers
//! a command is a FIXED CATALOG of session-local utilities whose whole effect is inside the
//! session itself (its output, its status, its variables and the in-memory tree its file
//! utilities work over). So this is not a shell with the host behind it: a command the
//! catalog does not name, and every command form and expansion the session does not perform,
//! FAILS CLOSED with the status a shell answers for a command it could not run. A declared
//! program therefore chooses between those utilities and nothing at all; it cannot choose a
//! program on the machine running the emulator.
//!
//! Every bound is the emulator's, held in code the program cannot reach: the session's own
//! budget on how many commands one run performs, how long and how deeply nested a line may
//! be, how far a word may expand and how much its tree may hold, beside
//! [`MAX_CONTAINER_OUTPUT_LINES`] on what one run may leave behind and
//! [`MAX_CONTAINER_RUNS_PER_DRAIN`] on how many programs one call performs. The work is
//! synchronous and CPU-bound, so it is performed off the request worker
//! ([`run_container_program_off_worker`]) and the worker serving the request is released
//! while it runs.
//!
//! The vector is handed to the session as ONE command line, quoted word by word by the
//! maintained POSIX word quoter (`shell-words`) so a word carrying a space, a quote or a
//! shell operator stays ONE word rather than becoming syntax. Its first word is the PROGRAM,
//! which a container command names by path (`/bin/sh`), and the session's catalog names a
//! utility by the name `PATH` resolution would find it under, so that program's final path
//! component is the command name the session is given. The variables the session starts with
//! are the job's own effective `environment` ("the environment variables to pass to a
//! container") and nothing of this process's.
//!
//! # A program nothing performed CLAIMS NOTHING
//!
//! Whether the declared program is one this emulator can perform at all is decided BEFORE it
//! is performed ([`performable_program`]), so a container declaring NO command — whose
//! program is the one inside the caller's own image — and one naming a program the catalog
//! does not hold are both answered [`ContainerRun::NotPerformed`], which carries no exit
//! status and no output. Nothing ran, so nothing of a run is reported: such a job ends in
//! the modelled failure status with a reason NAMING what was not performed, never with an
//! exit code, a `startedAt` or the reason a container that really exited carries.
//!
//! # When it is performed
//!
//! `STARTING` is where "the relevant container initiation operations are underway", so the
//! program of a job the scheduler has just placed is performed while it is there: once per
//! job ([`ComAmazonawsBatchAwsbatchv20160810State::jobs_awaiting_container_run`]), by the
//! call that finds it waiting and with this store's lock RELEASED
//! ([`settle_job_container_runs`], the shape [`super::image_pull`] and [`super::job_logs`]
//! follow). The outcome is then recorded on the job, and the clock materialises it: the move
//! into `RUNNING` and the terminal status one span later are both read off that one
//! recording, so what a job reports never depends on how quickly anybody called.

use super::*;

/// How many lines of what the container wrote one run keeps.
///
/// THIS NUMBER IS THE EMULATOR'S: the program is caller-supplied, so what it can leave
/// behind is bounded here.
pub const MAX_CONTAINER_OUTPUT_LINES: usize = 200;

/// How many bytes ONE kept line may carry, under [`MAX_CONTAINER_OUTPUT_LINES`]' rule
/// applied to one line. A longer line is truncated rather than dropped, so what the
/// container wrote is still reported.
pub const MAX_CONTAINER_OUTPUT_LINE_BYTES: usize = 1_024;

/// How many words one container's command may carry, and how many bytes one of those words
/// may be.
///
/// EMULATOR ceilings, stated here so the vector is bounded where it is BUILT rather than
/// only where it is performed: a vector past either one names no program at all.
pub const MAX_PROGRAM_WORDS: usize = 200;
/// See [`MAX_PROGRAM_WORDS`].
pub const MAX_PROGRAM_WORD_BYTES: usize = 4 * 1024;

/// How many container programs ONE drain performs.
///
/// This store's OWN bound: performing a program is CPU-bound work, and the jobs waiting for
/// one are only bounded by the jobs store itself ([`limits::MAX_JOBS`]), so one call never
/// performs more than this many. What a drain leaves behind keeps the clock pump awake
/// ([`ComAmazonawsBatchAwsbatchv20160810State::has_pending_container_runs`]), so nothing is
/// dropped — it is performed a tick later.
pub const MAX_CONTAINER_RUNS_PER_DRAIN: usize = 10;

/// The `statusReason` of a job whose container EXITED, whatever status it exited with.
///
/// A Batch job's container is an Amazon ECS task's container, and the words are that
/// service's own for a task whose essential container has exited — the sibling constant in
/// the Amazon ECS crate carries the same sentence for the task's `stoppedReason`. Fixed
/// text with no variable part, so it reports nothing about the job.
pub const ESSENTIAL_CONTAINER_EXITED_REASON: &str = "Essential container in task exited";

/// What one placed job's container program did, or the fact that nothing was performed for
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerRun {
    /// The declared program WAS performed: the status it exited with and the lines it wrote.
    Performed {
        /// The status the program exited with, as the session performed it — the job's
        /// `ContainerDetail$exitCode`.
        status: i32,
        /// The lines the program wrote, in the order it wrote them, bounded by
        /// [`MAX_CONTAINER_OUTPUT_LINES`]. They are the container's own output, so they are
        /// what its log stream holds ([`super::job_logs`]).
        output: Vec<String>,
    },
    /// NOTHING was performed for the container, and this is why. No status and no output are
    /// claimed, because none was produced.
    NotPerformed {
        /// The job's `statusReason`: what was not performed, in this emulator's own words,
        /// because no container exited.
        reason: String,
    },
}

impl ContainerRun {
    /// Whether a program was performed at all — which is what decides whether the job ever
    /// reaches `RUNNING` and reports an instant its work started.
    #[must_use]
    pub fn performed(&self) -> bool {
        matches!(self, Self::Performed { .. })
    }

    /// The `JobStatus` the job this run belongs to ends in, and the `statusReason` that goes
    /// with it.
    ///
    /// The exit status decides: zero is the job that succeeded, and every other status is a
    /// job that failed, each reported with the reason a container that exited carries
    /// ([`ESSENTIAL_CONTAINER_EXITED_REASON`]). A program nothing performed ends the job with
    /// the reason it carries instead.
    #[must_use]
    pub fn ending(&self) -> (&'static str, String) {
        match self {
            Self::Performed { status: 0, .. } => (
                JOB_SUCCEEDED,
                ESSENTIAL_CONTAINER_EXITED_REASON.to_string(),
            ),
            Self::Performed { .. } => (JOB_FAILED, ESSENTIAL_CONTAINER_EXITED_REASON.to_string()),
            Self::NotPerformed { reason } => (JOB_FAILED, reason.clone()),
        }
    }

    /// The exit status this run reports, and `None` for a run nothing performed.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::Performed { status, .. } => Some(*status),
            Self::NotPerformed { .. } => None,
        }
    }
}

/// How this emulator states that a job's container declared no program for it to run: the
/// image's own entrypoint is what AWS would start, and this emulator starts no image.
///
/// Not an AWS sentence, and deliberately not the reason a container that exited carries: no
/// container exited, so nothing about an exit status may be claimed. A job that declares a
/// command this emulator CAN perform is reported from that program's own outcome instead.
#[must_use]
pub fn no_declared_program(image: Option<&str>) -> String {
    let runs = match image {
        Some(image) => format!("the image \"{image}\""),
        None => "the job's container".to_string(),
    };
    format!(
        "This emulator starts no container image, so the program {runs} declares as its own \
         entrypoint was never started: declare a command this emulator performs."
    )
}

/// How this emulator states that the program a job's container DECLARED is not one it
/// performs: the container's program was never started, so no status is claimed for it.
#[must_use]
pub fn program_not_performed(command: &str) -> String {
    format!(
        "This emulator performs a job container's declared command itself and provides no \
         program named \"{command}\", so the container's program was never started and the \
         job performed no work."
    )
}

/// The argument vector a container runs: the command the job's effective container carries.
///
/// Empty when it carries none — a container running the program its IMAGE declares, which no
/// request here names — and empty too for a vector past this emulator's own ceilings
/// ([`MAX_PROGRAM_WORDS`], [`MAX_PROGRAM_WORD_BYTES`]): a caller supplies the words, so how
/// many of them there may be and how long each may be is decided here and not by the request.
#[must_use]
pub fn container_program(command: Option<&[String]>) -> Vec<String> {
    let declared: Vec<String> = command
        .unwrap_or_default()
        .iter()
        .take(MAX_PROGRAM_WORDS + 1)
        .cloned()
        .collect();
    if declared.len() > MAX_PROGRAM_WORDS
        || declared
            .iter()
            .any(|word| word.len() > MAX_PROGRAM_WORD_BYTES)
    {
        return Vec::new();
    }
    declared
}

/// The command name the session would be given for `program`, when that name is one the
/// session's catalog performs — and `None` for a vector naming a program it provides none
/// for, which is a program nothing starts.
///
/// A container command names its program by path (`/bin/sh`) and the catalog names a utility
/// by the name `PATH` resolution would find it under, so the final path component is the
/// command name. The catalog is the session's own: its built-in utilities, the text filters
/// it performs, and the file utilities a session working over a tree provides (which is how
/// `sh -c` is performed).
#[must_use]
pub fn performable_program(program: &[String]) -> Option<&str> {
    let executable = program.first()?;
    let name = executable.rsplit('/').next().unwrap_or(executable);
    let performed = ::service_common::posix_shell_session::BUILT_IN_COMMANDS.contains(&name)
        || ::service_common::posix_shell_session::VOLUME_COMMANDS.contains(&name)
        || ::service_common::posix_shell_text_filters::TEXT_FILTER_COMMANDS.contains(&name);
    performed.then_some(name)
}

/// Perform `program` — a container's argument vector — and report what it did, or that
/// nothing was performed for it.
///
/// `image` is the image the job's container runs, which the reason for a container declaring
/// no program at all names.
///
/// Pure computation over the caller's own words: the session starts no process, opens no host
/// file and reaches no network, and the work it performs is bounded by that session's own
/// bounds.
#[must_use]
pub fn run_container_program(
    program: &[String],
    image: &str,
    environment: &[(String, String)],
) -> ContainerRun {
    if program.is_empty() {
        return ContainerRun::NotPerformed {
            reason: no_declared_program((!image.is_empty()).then_some(image)),
        };
    }
    let Some(name) = performable_program(program) else {
        let declared = program.first().map_or("", String::as_str);
        return ContainerRun::NotPerformed {
            reason: program_not_performed(declared),
        };
    };
    let mut words: Vec<&str> = vec![name];
    words.extend(program.iter().skip(1).map(String::as_str));
    let line = ::shell_words::join(words);
    let variables: BTreeMap<String, String> = environment.iter().cloned().collect();
    let config = ::service_common::posix_shell_session::SessionConfig {
        output: Some(::service_common::posix_shell_session::OutputLimits {
            lines: MAX_CONTAINER_OUTPUT_LINES,
            line_bytes: MAX_CONTAINER_OUTPUT_LINE_BYTES,
        }),
        ..::service_common::posix_shell_session::SessionConfig::default()
    };
    // The container has storage of its own — the writable layer and the volumes a task is
    // given — so the session performs the file utilities over its own tree, which is also
    // what makes `sh -c` a program it can run.
    let mut session =
        ::service_common::posix_shell_session::ShellSession::new(variables, config)
            .with_file_utilities();
    let status = session.run(&line);
    // An `exit` the program performed is the status it ended with, the way a shell takes it.
    let status = session.exit_status().unwrap_or(status);
    ContainerRun::Performed {
        status,
        output: session.output().to_vec(),
    }
}

/// [`run_container_program`] off the request worker: performing a caller's own program is
/// synchronous, CPU-bound work bounded by the session's own command budget, so it goes to the
/// blocking pool and the worker serving the request is released while it runs.
///
/// `None` is a fault of this process rather than of the request — the work could not be
/// performed at all — so nothing is recorded for it and the next drain attempts it again.
pub async fn run_container_program_off_worker(
    program: &[String],
    image: &str,
    environment: &[(String, String)],
) -> Option<ContainerRun> {
    let program = program.to_vec();
    let image = image.to_string();
    let environment = environment.to_vec();
    ::tokio::task::spawn_blocking(move || run_container_program(&program, &image, &environment))
        .await
        .ok()
}

/// What ONE job's container run needs: the job it belongs to, the program its container
/// declares, the image that container runs and the environment it was given.
#[derive(Debug, Clone)]
pub struct JobContainerRun {
    /// The job the outcome belongs to.
    pub job_id: String,
    /// The argument vector its container runs ([`container_program`]).
    pub program: Vec<String>,
    /// The image its container runs, as `ContainerDetail$image` holds it.
    pub image: String,
    /// The environment variables the job's effective container carries, in the order they
    /// were merged.
    pub environment: Vec<(String, String)>,
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The placed jobs whose container program has not been performed yet.
    ///
    /// A job is one of them from the instant the placement put it into `STARTING` — where the
    /// container initiation is underway — until its outcome is recorded, and only when the
    /// image its container runs was there to be fetched: a fetch the registry refused ended
    /// the initiation before any container was created, so there is no program to perform.
    /// Costs one walk of the jobs map, which [`limits::MAX_JOBS`] bounds, and answers at most
    /// [`MAX_CONTAINER_RUNS_PER_DRAIN`] of them.
    pub fn jobs_awaiting_container_run(&self) -> Vec<JobContainerRun> {
        self.jobs
            .values()
            .filter(|job| {
                job.status == JOB_STARTING
                    && job.container_run.is_none()
                    && matches!(job.image_fetch, Some(ImageFetch::Served) | None)
            })
            .filter_map(|job| {
                let container = job.container.as_ref()?;
                Some(JobContainerRun {
                    job_id: job.job_id.clone(),
                    program: container_program(container.command.as_deref()),
                    image: container.image.clone().unwrap_or_default(),
                    environment: container
                        .environment
                        .iter()
                        .flatten()
                        .filter_map(|pair| {
                            Some((pair.name.clone()?, pair.value.clone().unwrap_or_default()))
                        })
                        .collect(),
                })
            })
            .take(MAX_CONTAINER_RUNS_PER_DRAIN)
            .collect()
    }

    /// Record what each job's container program did.
    ///
    /// Written once per job: a job that already carries an outcome keeps it, so a later call
    /// re-performs nothing and what a job reports cannot change under a reader.
    pub fn record_container_runs(&mut self, outcomes: Vec<(String, ContainerRun)>) {
        for (id, outcome) in outcomes {
            if let Some(job) = self.jobs.get_mut(&id) {
                if job.container_run.is_none() {
                    job.container_run = Some(outcome);
                }
            }
        }
    }

    /// Whether a placed job is still waiting for its container program to be performed, which
    /// is what keeps the clock pump awake until every job can take its next move.
    pub(super) fn has_pending_container_runs(&self) -> bool {
        !self.jobs_awaiting_container_run().is_empty()
    }

    /// What `id`'s container program did, or `None` while it has not been performed.
    ///
    /// A job whose container this store holds none of has no program to perform — nothing of
    /// a container job is stored for it — so nothing was performed for it either.
    pub(super) fn container_run_of(&self, id: &str) -> Option<ContainerRun> {
        let job = self.jobs.get(id)?;
        if job.container.is_none() {
            return Some(ContainerRun::NotPerformed {
                reason: no_declared_program(None),
            });
        }
        job.container_run.clone()
    }

    /// Move `id` into `RUNNING` at `at`, owing its end one span later.
    ///
    /// `JobStatus$RUNNING` is where the job's container is running the program it declared,
    /// and `JobDetail$startedAt` is "when the job transitioned from the `STARTING` state to
    /// the `RUNNING` state" — which is also what `AttemptDetail$startedAt` records for the
    /// attempt that placement started, so the two report one instant.
    ///
    /// The lines the program wrote are delivered from here, while the job is RUNNING: a
    /// container writes its output as it runs rather than when it exits, so a client watching
    /// the job's log stream finds them before the job reaches its terminal status.
    pub(super) fn start_running_job(&mut self, id: &str, at: i64) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.started_at = Some(at);
            if let Some(attempt) = job
                .attempts
                .last_mut()
                .filter(|attempt| attempt.ended_at.is_none())
            {
                attempt.started_at = Some(at);
            }
        }
        self.queue_container_output(id, at);
        self.set_job_status(id, JOB_RUNNING, at);
        self.set_job_due(id, Some(at + JOB_PHASE_MILLIS));
    }

    /// End `id` at `at` with what its container's program did: the status that program
    /// exited with decides the job's own, and the exit code is reported on the container and
    /// on the attempt that ran it.
    ///
    /// What the program WROTE was delivered when the job entered `RUNNING`
    /// ([`Self::start_running_job`]), as a container's output is.
    pub(super) fn end_running_job(&mut self, id: &str, run: &ContainerRun, at: i64) {
        let (status, reason) = run.ending();
        let code = run.exit_code();
        if let Some(job) = self.jobs.get_mut(id) {
            if let Some(container) = job.container.as_mut() {
                container.exit_code = code;
            }
            if let Some(attempt) = job
                .attempts
                .last_mut()
                .filter(|attempt| attempt.ended_at.is_none())
            {
                attempt.exit_code = code;
            }
        }
        self.finish_job(id, status, Some(&reason), at);
    }

    /// Queue the lines `id`'s container wrote, for delivery to the log stream the `awslogs`
    /// driver opened for its task ([`super::job_logs`]).
    ///
    /// The destination is the one the placement decided — the driver's own options over the
    /// stream that placement named — so the events land in the stream the job reports and
    /// nowhere else. A container that wrote nothing queues nothing, and a container whose
    /// output goes to another log driver has no CloudWatch Logs stream to deliver to.
    pub(super) fn queue_container_output(&mut self, id: &str, started_at: i64) {
        let Some(job) = self.jobs.get(id) else {
            return;
        };
        let Some(ContainerRun::Performed { output, .. }) = job.container_run.as_ref() else {
            return;
        };
        if output.is_empty() {
            return;
        }
        let events: Vec<(i64, String)> = output
            .iter()
            .enumerate()
            .map(|(index, line)| {
                (
                    started_at.saturating_add(i64::try_from(index).unwrap_or_default()),
                    line.clone(),
                )
            })
            .collect();
        let region = arn_region(&job.job_arn).unwrap_or(DEFAULT_REGION).to_string();
        let configuration = job
            .container
            .as_ref()
            .and_then(|container| container.log_configuration.clone());
        let Some(stream) = job
            .attempts
            .last()
            .and_then(|attempt| attempt.log_stream_name.clone())
        else {
            return;
        };
        let Some(target) = job_log_target(configuration.as_ref(), &stream, &region) else {
            return;
        };
        self.queue_log_stream(JobLogStream { events, ..target });
    }
}

/// Perform the container program of every placed job waiting for one, and record what each
/// one did.
///
/// Called by the drain every operation of this service's job plane makes, and by the clock
/// pump, with this store's guard dropped: performing a caller's program is CPU-bound work, so
/// no lock of this store is held while it runs. Nothing is performed twice — a job that
/// already carries an outcome is not collected.
pub async fn settle_job_container_runs<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The guard is taken and dropped inside this statement: nothing of this store is held
    // while the programs below are performed.
    let waiting = state.lock().jobs_awaiting_container_run();
    if waiting.is_empty() {
        return;
    }
    let mut outcomes: Vec<(String, ContainerRun)> = Vec::with_capacity(waiting.len());
    for job in waiting {
        if let Some(outcome) =
            run_container_program_off_worker(&job.program, &job.image, &job.environment).await
        {
            outcomes.push((job.job_id, outcome));
        }
    }
    state.lock().record_container_runs(outcomes);
}
