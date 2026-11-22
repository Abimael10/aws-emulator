// SPDX-License-Identifier: CC0-1.0

//! RUNNING the workload a job run submitted: where its code comes from, what runs it, and
//! what the run comes to rest in.
//!
//! "A job run is a unit of work, such as a Spark JAR, Hive query, or SparkSQL query, that you
//! submit to an Amazon EMR Serverless application" (`JobRun`), and `SparkSubmit$entryPoint` is
//! "The entry point for the Spark submit job run". So a job run IS one execution of the work
//! its driver named, and the terminal `JobRunState` it reports is whatever that execution came
//! to — `SUCCESS` for work that ran to its end, `FAILED` for work that raised or was stopped.
//!
//! For the one class of submission this emulator can run — a `sparkSubmit` of a Python
//! application in Amazon S3 — that outcome is produced here, by really running it:
//!
//! * the application's code is READ from the emulator's own Amazon S3, over the framework's
//!   in-process callback ([`smithy_server_runtime::OperationContext::callback`]) and signed as
//!   a session of the run's own `executionRoleArn` — the same transport and the same identity a
//!   client's own S3 request takes, so the bytes that run are the bytes that were uploaded and
//!   a role that may not read them is refused by the object store itself;
//! * it is EXECUTED by the contained Python interpreter this workspace already embeds (the
//!   Lambda service crate's `state::execution::python`), with the ONE in-process Spark runtime
//!   this workspace supplies ([`::service_common::spark_runtime`]) and the AWS SDK modules that
//!   interpreter puts on a guest's import path — so the application's own transformations are
//!   COMPUTED and its own writes are really made, into this emulator's object store, as the
//!   role the run executes as. An object the application writes under an `s3://` output prefix
//!   is an object a later `GetObject` really reads;
//! * what the RUN reports is what that execution answered: an application that ran to its end
//!   reaches `SUCCESS`, one that raised reaches `FAILED` carrying its own exception in
//!   `stateDetails`, and one stopped by a bound of this emulator reaches `FAILED` saying which
//!   bound. Nothing is ever reported as a success nothing computed.
//!
//! A driver this emulator runs NOTHING for — a Hive query, an entry point that is not a Python
//! application — is answered at once and says so ([`JOB_RUN_NOT_RUNNABLE_LEAD`]), rather than
//! reporting an outcome no engine produced.
//!
//! # When it runs
//!
//! The workload is owed from the instant the submit was ACCEPTED, whoever submitted it: the run
//! is marked [`JobRunRecord::awaits_workload`] and the execution is STARTED by `StartJobRun`
//! itself and, for anything it did not reach, by the next call that settles this store
//! ([`perform_owed`]). A submit waits for none of it and a read waits only
//! [`WORKLOAD_FIRST_WAIT`], so no client's call is held for as long as the application takes
//! and the run is observable while its workload is in flight. What an execution nobody is
//! waiting for any more came to is recorded by the TASK performing it
//! ([`perform_handed_off_workload`]), so a run comes to rest whether or not anybody polls it.
//!
//! WHAT the run then reports is the CLOCK's: the state the execution came to is due once every
//! stage the run still owes has fallen due ([`super::JOB_RUN_STAGES`]) and never before the
//! execution itself answered ([`JobRunRecord::record_execution`]) — so a run passes through
//! `RUNNING` for as long as its workload takes and a client polling it observes every state it
//! goes through. The instant the execution answered is the instant that state is dated from,
//! whether a call was waiting for it or the task performing it recorded it, so what a run comes
//! to rest in — and when — does not depend on when anybody read it.
//!
//! # Bounds and containment
//!
//! The submitted application is caller-supplied code, so every bound is this emulator's and
//! none of them is reachable from the application: how many run at once
//! ([`MAX_CONCURRENT_WORKLOAD_RUNS`], whose permit is taken before anything is spawned and
//! travels into the thread that runs it), how many ONE call starts
//! ([`MAX_WORKLOAD_RUNS_PER_CALL`] over at most [`WORKLOAD_CALL_CEILING`] of that call's own
//! time), how large the application may be ([`MAX_APPLICATION_BYTES`], enforced by the ranged
//! read), the span of the RUN's own time it may last — the caller's own
//! `executionTimeoutMinutes` clamped DOWN to this emulator's ceiling — and the step, memory,
//! recursion and output ceilings the interpreter is given ([`workload_budget`]), and the HOST
//! time one run may take beside them ([`WORKLOAD_HOST_CEILING`], which the engine STOPS the run
//! on whatever its own clock reads). The containment — the native modules that are never
//! registered, the host surface that is compiled out and the stop no guest call can clear — is
//! that engine's, which is why nothing of it is stood up again here.
//!
//! An execution this service began stays reachable rather than being forgotten: the TASK that
//! waits for its answer and records what it came to is held on this store
//! ([`ComAmazonawsEmrserverlessAwstoledowebserviceState::in_flight_workloads`]) until it has
//! ended, and this emulator gives up on an execution that has not answered within
//! [`WORKLOAD_HOST_WAIT`] — the host ceiling the engine stops it on, plus the grace that stop
//! needs to land — settling the run in the modelled failure state rather than leaving it
//! `RUNNING`.

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::aws_api::CallerIdentity;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::execution_role_session::execution_role_identity;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    function_sdk_access, AwsAccess, INIT_PHASE_LIMIT,
};
use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

use super::*;
use crate::ops::types::JobDriver;

/// How large an application file this emulator reads.
///
/// CHOSEN, not an AWS limit: the file is an object a caller uploaded, so what one run takes
/// into this process is bounded here rather than by the object's own size. The bound is
/// enforced by the RANGED read, so nothing larger is ever buffered.
pub const MAX_APPLICATION_BYTES: usize = 1024 * 1024;

/// The longest ONE run of a submitted application may last, in the RUN's own time.
///
/// CHOSEN, not an AWS duration: `JobRun$executionTimeoutMinutes` is the caller's own bound and
/// is stated in minutes (defaulting to 720), so it is clamped DOWN to this before it bounds
/// anything ([`workload_budget`]) — this is the span this emulator gives one submitted
/// application, and a run past it reaches the modelled failure state saying so.
///
/// It is a span of the RUN's own time rather than of this host's: it is spent by the waits the
/// application asks for and by one tick per traced event it delivers ([`WORKLOAD_CLOCK_TICK`]),
/// and by nothing of this host's speed — so what a run comes to rest in is the same on an idle
/// host and on a loaded one. What the run may take of THIS host is bounded beside it
/// ([`WORKLOAD_HOST_CEILING`]), and no request waits for either.
pub const WORKLOAD_RUN_CEILING: Duration = Duration::from_secs(60);

/// How far ONE reading of a workload run's clock moves it.
///
/// The span above is spent by what the RUN does, and a traced event is one of those: the engine
/// reads the run's clock once per event the application delivers and once per sample the host's
/// watch takes over it. The tick is sized so that the whole step budget
/// ([`MAX_WORKLOAD_STEPS`]) spends well under that span — which is what leaves an application
/// that computes without end to be answered by the step budget, a count of its OWN work, and
/// leaves the span itself for the waits an application asks for.
pub const WORKLOAD_CLOCK_TICK: Duration = Duration::from_nanos(100);

/// How much of THIS HOST's own time ONE run of an application may take, past which the engine
/// stops it whatever its own clock reads.
///
/// CHOSEN: the span above is the RUN's own time, so it bounds no CPU of this host — an
/// application spending traced events, or sitting in one long native call that delivers none,
/// would hold a thread of this process for as long as the host took over it. This is the bound
/// that stops it: the engine reads it from this host's own clock at every traced event and from
/// the host's own watch over the run ([`python::Clock::within`]).
///
/// It is generous beside the spans the engine's phases take in the run's own time
/// ([`INIT_PHASE_LIMIT`] standing the execution environment up, then [`WORKLOAD_RUN_CEILING`]
/// of application): the interpreter traces every event it executes, so an aggregate computed
/// over an input of the size a client really submits takes this host several times as long as
/// the run's own span, and an application that FINISHED must be reported on what it came to
/// rather than on how fast this host read it.
pub const WORKLOAD_HOST_CEILING: Duration = INIT_PHASE_LIMIT
    .saturating_add(WORKLOAD_RUN_CEILING)
    .saturating_mul(HOST_CEILING_OVER_THE_ENGINE_SPAN);

/// How many times the engine's own spans the host ceiling allows for
/// ([`WORKLOAD_HOST_CEILING`]).
const HOST_CEILING_OVER_THE_ENGINE_SPAN: u32 = 3;

/// How long the engine's own stop is given to land, once the host ceiling has latched it.
///
/// CHOSEN: the stop is raised at the next traced event, which a running application delivers at
/// once; this is what the wait allows for it before it stops waiting.
pub const WORKLOAD_STOP_GRACE: Duration = Duration::from_secs(5);

/// How long this emulator waits, in all, for the thread one application runs on.
///
/// A thread that has still not answered by then is given up on and the run reports that this
/// emulator has no outcome for it ([`job_run_workload_unanswered`]). The thread keeps its permit
/// until it ends, which the ceiling above is what bounds.
pub const WORKLOAD_HOST_WAIT: Duration =
    WORKLOAD_HOST_CEILING.saturating_add(WORKLOAD_STOP_GRACE);

/// How long the call that STARTS a run waits for it before leaving it to run on.
///
/// CHOSEN: short, because the application is caller-supplied code and no client's call may be
/// held for it. An application that answers inside this slice decides its run on the call that
/// started it — which is every application small enough to finish at once — and one that does
/// not is left in flight, with the run still `RUNNING`, while the rest of the wait is carried by
/// a task of this process.
pub const WORKLOAD_FIRST_WAIT: Duration = Duration::from_secs(2);

/// How many submitted applications run at once in this process, which is also how many
/// handed-off executions can be in flight at a time.
///
/// CHOSEN: a run is an interpreter, a thread and the memory the two hold, so the number of them
/// this process carries is bounded rather than following from how many job runs an account
/// submits. A run that finds no free interpreter is handed off by NOBODY — the permit is taken
/// before anything is spawned — so it stays owed and a later call hands it off.
pub const MAX_CONCURRENT_WORKLOAD_RUNS: usize = 8;

/// The permits [`MAX_CONCURRENT_WORKLOAD_RUNS`] hands out, for the whole process.
///
/// Taken with no wait at all, BEFORE an execution is handed off, and held as an owned permit
/// MOVED INTO the thread that runs the application — so it is given back when that thread really
/// ends, including for a run nobody is waiting for any more.
static WORKLOAD_RUN_PERMITS: ::std::sync::LazyLock<::std::sync::Arc<::tokio::sync::Semaphore>> =
    ::std::sync::LazyLock::new(|| {
        ::std::sync::Arc::new(::tokio::sync::Semaphore::new(MAX_CONCURRENT_WORKLOAD_RUNS))
    });

/// How many submitted applications ONE call starts, leaving the rest to the next call.
///
/// CHOSEN: the ceiling on what one REQUEST costs, beside the ceiling on what one run costs.
pub const MAX_WORKLOAD_RUNS_PER_CALL: usize = 8;

/// How long ONE call spends starting owed workloads before it leaves the rest to the next
/// call.
///
/// CHOSEN: a further workload is started only while a whole [`WORKLOAD_FIRST_WAIT`] still fits
/// inside this span, so a call is held for this long at most whatever the store owes.
pub const WORKLOAD_CALL_CEILING: Duration = Duration::from_secs(5);

/// The stack an application's thread is given: well above the interpreter's clamped recursion
/// limit, so deep recursion in an application ends that run with the interpreter's own
/// `RecursionError` rather than touching the thread's guard page.
const WORKLOAD_STACK_BYTES: usize = 16 * 1024 * 1024;

/// How many traced events one application may execute.
///
/// CHOSEN: large enough for the Spark programs a client runs here — a program resolves its
/// imports against the `pyspark` runtime this emulator supplies, which is itself Python, so
/// standing those modules up and then running the program's own work both spend from this
/// budget — and small enough that a loop with no exit ends in a small part of the host time one
/// run may take ([`WORKLOAD_HOST_CEILING`]). It is a count of the application's OWN work, so it
/// is the same count on an idle host and on a loaded one.
pub const MAX_WORKLOAD_STEPS: u64 = 2_000_000;

/// How much memory one application may add to this process. CHOSEN, for the same reason.
const MAX_WORKLOAD_MEMORY_BYTES: u64 = 512 * 1024 * 1024;

/// How much one application may write to the `/tmp` of the environment it runs in. CHOSEN, for
/// the same reason: that directory is a worker's disk on a real application and is held in this
/// process's memory here, so what one run leaves there is bounded like everything else it holds.
const MAX_WORKLOAD_SCRATCH_BYTES: u64 = 512 * 1024 * 1024;

/// How deep one application may recurse. The interpreter's own default is below what the frozen
/// standard library's import chain needs, so this is the limit a run is given.
const MAX_WORKLOAD_RECURSION: usize = 2_000;

/// How much of an application's output is kept, and how large a value it may answer with.
/// Neither reaches the wire: a job run reports no program output.
const MAX_WORKLOAD_LOG_BYTES: usize = 64 * 1024;
/// See [`MAX_WORKLOAD_LOG_BYTES`].
const MAX_WORKLOAD_ANSWER_BYTES: usize = 64 * 1024;

/// The ROLE-SESSION NAME a job run's session of its `executionRoleArn` is opened under.
///
/// CHOSEN: nothing this service models records the name EMR Serverless opens a job runner's
/// session under, so this one names the service and the kind of session and never a value of one
/// run — the principal every run of every application speaks for is the same shape.
pub const JOB_RUN_ROLE_SESSION_NAME: &str = "EMRServerlessJobRunSession";

/// The name the credential a job run's own calls are signed with is reported under, for a client
/// of the credential store that asks where a credential came from.
const JOB_RUN_CREDENTIAL_SOURCE: &str = "emr-serverless-job-run-execution-role";

/// The file extension the one class of application this emulator RUNS is written as.
///
/// `SparkSubmit$entryPoint` is an Amazon S3 path, and a Spark submit's entry point is either a
/// Python application or a JAR. This emulator embeds a Python interpreter and no Java virtual
/// machine, so a Python application is the one it runs; anything else is answered by
/// [`job_run_entry_point_not_runnable`] rather than by an outcome no engine produced.
pub const PYTHON_APPLICATION_EXTENSION: &str = ".py";

/// The fixed words a `stateDetails` leads with for a run whose work nothing here runs.
pub const JOB_RUN_NOT_RUNNABLE_LEAD: &str = "Job run not executed:";

/// `JobRun$stateDetails` of a run whose `jobDriver` names a Hive query.
///
/// This emulator runs the Spark submit of a Python application and embeds no Hive, so a run
/// submitted with that driver says nothing ran it.
#[must_use]
pub fn job_run_hive_not_runnable() -> String {
    format!(
        "{JOB_RUN_NOT_RUNNABLE_LEAD} this emulator runs a Spark submit of a Python application \
         and executes no Hive query."
    )
}

/// `JobRun$stateDetails` of a run whose Spark submit names an entry point no engine here runs.
#[must_use]
pub fn job_run_entry_point_not_runnable() -> String {
    format!(
        "{JOB_RUN_NOT_RUNNABLE_LEAD} this emulator runs a Spark submit whose entry point is a \
         Python application in Amazon S3."
    )
}

/// The fixed words a `stateDetails` leads with for a run whose application could not be read.
///
/// It names no LOCATION: `SparkSubmit$entryPoint` is `@sensitive`, so the path a caller
/// submitted is not written into a state detail this service also publishes into a run's log
/// artifacts ([`super::job_run_logs`]). What the detail carries is what the object store
/// ANSWERED about it, which is the part a caller cannot already read off its own submit.
pub const JOB_RUN_UNREADABLE_LEAD: &str =
    "Job run failed: unable to download the application the Spark submit's entry point names";

/// `JobRun$stateDetails` of a run whose application could not be read from the entry point its
/// Spark submit names: the fixed text above and what the object store answered.
#[must_use]
pub fn job_run_application_unreadable(detail: &str) -> String {
    format!("{JOB_RUN_UNREADABLE_LEAD}: {detail}")
}

/// `JobRun$stateDetails` of a run whose execution role may not READ the object its entry point
/// names.
#[must_use]
pub fn job_run_application_not_accessible() -> String {
    format!("{JOB_RUN_UNREADABLE_LEAD}: access to the object was denied.")
}

/// The fixed words the `stateDetails` of a run whose application FAILED leads with.
pub const JOB_RUN_FAILED_LEAD: &str = "Job run failed:";

/// `JobRun$stateDetails` of a run whose application raised: the exception's own type and
/// message, as the interpreter reported them, which is the application's failure and not this
/// emulator's words for it.
#[must_use]
pub fn job_run_application_failed(error_type: &str, error_message: &str) -> String {
    if error_message.is_empty() {
        return format!("{JOB_RUN_FAILED_LEAD} {error_type}");
    }
    format!("{JOB_RUN_FAILED_LEAD} {error_type}: {error_message}")
}

/// `JobRun$stateDetails` of a run stopped because it ran past one of this emulator's own bounds
/// on the application — the span one run may last, its step budget, its memory ceiling or the
/// size of the value it answered.
#[must_use]
pub fn job_run_bound_reached(bound: &str) -> String {
    format!("{JOB_RUN_FAILED_LEAD} the application was stopped because it ran past {bound}.")
}

/// `JobRun$stateDetails` of a run whose application's thread had still not answered when the
/// task performing it gave up waiting ([`WORKLOAD_HOST_WAIT`]), or that the engine stopped at
/// the host time one run may take.
///
/// Nothing of the application's own span ran out, so the text says this emulator has no outcome
/// for the run rather than claiming the application was stopped for taking too long.
#[must_use]
pub fn job_run_workload_unanswered() -> String {
    format!(
        "{JOB_RUN_FAILED_LEAD} the application's execution did not answer, so this emulator has \
         no outcome to report for the job run."
    )
}

/// `JobRun$stateDetails` of a run that ran to its end.
///
/// RECORDED behaviour: EMR Serverless reports the state details of a SUCCESSFUL run as the
/// EMPTY string — there is no detail beyond the state itself — while `String256`, the shape
/// the member targets, declares `@length(min: 1)` with `@pattern(".*\\S.*")`. The wire
/// behaviour is what is reproduced here: a client that logs or parses this member reads what
/// the service sends. Every run that did NOT run to its end carries a sentence saying what
/// happened instead ([`WorkloadOutcome::Failed`]).
pub const SUCCESS_STATE_DETAILS: &str = "";

/// `JobRun$stateDetails` while the run's own work is executing.
pub const RUNNING_STATE_DETAILS: &str = "Job run is executing on the application.";

/// What ONE execution of a job run's workload came to, which is what the state the clock owes
/// the run reports ([`JobRunRecord::record_execution`]).
///
/// `JobRunState` is `SUBMITTED | PENDING | SCHEDULED | RUNNING | SUCCESS | FAILED | CANCELLING |
/// CANCELLED | QUEUED`, so the two states an execution can leave a run in are `SUCCESS` and
/// `FAILED`: there is no third value for work this emulator could not carry out, which is why
/// every such answer is a `FAILED` whose `stateDetails` says what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkloadOutcome {
    /// The application ran and exited cleanly.
    Succeeded,
    /// The application raised, could not be read, or was stopped — and this is what the run
    /// reports as its `stateDetails`.
    Failed(String),
}

impl WorkloadOutcome {
    /// The `JobRunState` a run that came to this comes to rest in.
    #[must_use]
    pub fn job_run_state(&self) -> &'static str {
        match self {
            Self::Succeeded => "SUCCESS",
            Self::Failed(_) => "FAILED",
        }
    }

    /// The `JobRun$stateDetails` it carries once it has come to rest, held inside `String256`'s
    /// modelled `@length(max: 256)`.
    ///
    /// A run that SUCCEEDED carries what the service sends for one, which is nothing at all
    /// ([`SUCCESS_STATE_DETAILS`]); every other outcome carries the sentence that says what
    /// happened.
    #[must_use]
    pub fn state_details(&self) -> String {
        match self {
            Self::Succeeded => SUCCESS_STATE_DETAILS.to_string(),
            Self::Failed(details) => held_to_state_details(details),
        }
    }
}

/// `text` held inside `String256`'s modelled `@length(min: 1, max: 256)`, cut on a character
/// boundary.
///
/// A failure whose text says nothing is one nothing is known about, so it carries the sentence
/// for an execution that answered nothing rather than an empty detail.
fn held_to_state_details(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return job_run_workload_unanswered();
    }
    if trimmed.chars().count() <= STATE_DETAILS_MAX_CHARS {
        return trimmed.to_string();
    }
    trimmed
        .chars()
        .take(STATE_DETAILS_MAX_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// `String256`'s modelled `@length(max: 256)`, which every `stateDetails` this module words
/// stays inside.
pub const STATE_DETAILS_MAX_CHARS: usize = 256;

/// Everything one execution of a job run's workload needs, copied out of the store under its
/// guard so the execution itself holds none.
#[derive(Debug, Clone)]
pub struct JobRunWorkload {
    /// The application the run was submitted to.
    pub application_id: String,
    /// The run whose workload this is.
    pub job_run_id: String,
    /// `SparkSubmit$entryPoint`: "The entry point for the Spark submit job run."
    pub entry_point: String,
    /// `SparkSubmit$entryPointArguments`: "The arguments for the Spark submit job run", which
    /// the application reads off its own command line.
    pub entry_point_arguments: Vec<String>,
    /// `JobRun$executionRole`: the role the run's reads and writes are made as.
    pub execution_role_arn: String,
    /// The version of Spark the run's Amazon EMR release ships, which is what the application
    /// is told it is running on — `None` for a release nothing is known about.
    pub spark_version: Option<String>,
    /// `JobRun$executionTimeoutMinutes`, as the run carries it.
    pub execution_timeout_minutes: i64,
}

/// What one claimed job run owes: work this emulator runs, or an answer it can give without
/// running anything.
#[derive(Debug, Clone)]
enum ClaimedWorkload {
    /// A `sparkSubmit` of a Python application: the one class this emulator executes.
    Runnable(JobRunWorkload),
    /// A driver nothing here runs, answered at once and without anything being executed.
    NotRunnable {
        application_id: String,
        job_run_id: String,
        outcome: WorkloadOutcome,
    },
}

/// The version of Spark a run's Amazon EMR release ships, read from the Amazon EMR release
/// catalog this workspace already keeps.
///
/// `JobRun$releaseLabel` is "The Amazon EMR release associated with the application your job is
/// running on", so the engine versions it ships are the Amazon EMR release's own — the catalog
/// Amazon EMR's own service crate publishes for `DescribeReleaseLabel` — rather than a second
/// table here. A label that catalog knows nothing about tells the application nothing.
#[must_use]
pub fn spark_version_of(release_label: &str) -> Option<&'static str> {
    ::service_com_amazonaws_emr_elasticmapreduce::state::release_application_version(
        release_label,
        SPARK_APPLICATION_NAME,
    )
}

/// The name Amazon EMR's release catalog publishes the Spark engine under.
const SPARK_APPLICATION_NAME: &str = "Spark";

impl JobRunRecord {
    /// Whether this run still owes the work its driver submitted: nothing has answered for it
    /// and no execution of it is in flight.
    ///
    /// A run that has come to rest owes nothing — a cancel is an answer about the work, so a
    /// cancelled run is never executed afterwards.
    #[must_use]
    pub fn awaits_workload(&self) -> bool {
        self.workload_owed
            && !self.workload_claimed
            && !is_terminal_job_run_state(&self.job_run.state)
    }

    /// The workload this run owes, or the answer this emulator gives without running anything.
    fn claimed_workload(&self) -> ClaimedWorkload {
        let not_runnable = |outcome| ClaimedWorkload::NotRunnable {
            application_id: self.job_run.application_id.clone(),
            job_run_id: self.job_run.job_run_id.clone(),
            outcome,
        };
        let JobDriver::sparkSubmit(submit) = &self.job_run.job_driver else {
            return not_runnable(WorkloadOutcome::Failed(job_run_hive_not_runnable()));
        };
        let entry_point = submit.entry_point.trim().to_string();
        if !entry_point
            .to_ascii_lowercase()
            .ends_with(PYTHON_APPLICATION_EXTENSION)
        {
            return not_runnable(WorkloadOutcome::Failed(job_run_entry_point_not_runnable()));
        }
        ClaimedWorkload::Runnable(JobRunWorkload {
            application_id: self.job_run.application_id.clone(),
            job_run_id: self.job_run.job_run_id.clone(),
            entry_point,
            entry_point_arguments: submit.entry_point_arguments.clone().unwrap_or_default(),
            execution_role_arn: self.job_run.execution_role.clone(),
            spark_version: spark_version_of(&self.job_run.release_label).map(str::to_string),
            execution_timeout_minutes: self
                .job_run
                .execution_timeout_minutes
                .unwrap_or(DEFAULT_EXECUTION_TIMEOUT_MINUTES),
        })
    }

    /// Record what ONE execution of this run's workload came to.
    ///
    /// The state it decided is OWED by the clock rather than applied here: it falls due one
    /// stage span ([`JOB_RUN_STAGE_TRANSITION`]) after the later of the last stage the run
    /// still owes and the instant the execution answered — so a run is `RUNNING` for at least
    /// that span, and for as long as its work takes, and a client polling it observes every
    /// state it goes through. A run that has already come to rest — one a caller cancelled
    /// while the execution was in flight — keeps the state it reached: the answer is dropped
    /// rather than overwriting it.
    pub fn record_execution(&mut self, outcome: &WorkloadOutcome, answered_at: SystemTime) {
        let was_owed = self.workload_owed;
        self.workload_owed = false;
        self.workload_claimed = false;
        if !was_owed || is_terminal_job_run_state(&self.job_run.state) {
            return;
        }
        let owed = self
            .pending
            .iter()
            .map(|pending| pending.due)
            .max()
            .unwrap_or(answered_at);
        self.pending.push_back(PendingState {
            due: due_at(owed.max(answered_at), JOB_RUN_STAGE_TRANSITION),
            state: outcome.job_run_state().to_string(),
            state_details: Some(outcome.state_details()),
        });
    }

    /// Give the claim on this run's execution back, for an execution nothing performed, so a
    /// later call hands it off rather than anything reporting an outcome nothing produced.
    pub fn release_workload_claim(&mut self) {
        self.workload_claimed = false;
    }
}

impl ComAmazonawsEmrserverlessAwstoledowebserviceState {
    /// Claim the next job run that still owes its workload, so two calls that find the same run
    /// owed cannot both run it.
    fn claim_job_run_awaiting_workload(&mut self) -> Option<ClaimedWorkload> {
        for application in self.applications.values_mut() {
            for record in application.job_runs.values_mut() {
                if !record.awaits_workload() {
                    continue;
                }
                record.workload_claimed = true;
                return Some(record.claimed_workload());
            }
        }
        None
    }

    /// Keep an execution this process HANDED OFF, so one this service began stays reachable
    /// rather than detached, and is forgotten once the task performing it has ended.
    fn hold_in_flight_workload(
        &mut self,
        application_id: &str,
        job_run_id: &str,
        pending: PendingWorkloadRun,
    ) {
        self.in_flight_workloads
            .insert(in_flight_key(application_id, job_run_id), pending);
    }

    /// Forget the executions whose task has ENDED, which is every execution that has already
    /// recorded what it came to ([`perform_handed_off_workload`]).
    ///
    /// Nothing is decided here: the outcome of an execution is recorded by the task performing
    /// it, at the instant that execution ANSWERED, so what a run comes to rest in does not
    /// depend on when anybody next read it. This only drops the handles of the tasks that are
    /// done, so the table holds the executions still in flight and nothing else.
    fn forget_ended_workloads(&mut self) {
        self.in_flight_workloads
            .retain(|_, pending| !pending.task.is_finished());
    }
}

/// An execution this process started and has not finished waiting for.
///
/// The run itself is a thread of this process ([`run_application`]) and the wait for it is a
/// TASK of this process ([`perform_handed_off_workload`]); what is held here is that task's
/// handle, so an execution this service began stays reachable rather than detached.
#[derive(Debug)]
pub struct PendingWorkloadRun {
    /// The task waiting for the execution's own answer and recording what it came to.
    task: ::tokio::task::JoinHandle<()>,
}

/// What separates the two halves of the key an in-flight execution is held under: a unit
/// separator, which neither an `ApplicationId` nor a `JobRunId` admits (both are
/// `^[0-9a-z]+$`), so the pair is recovered from it unambiguously.
const IN_FLIGHT_KEY_SEPARATOR: char = '\u{1f}';

/// The key the execution of ONE job run is held under while it is in flight.
#[must_use]
pub fn in_flight_key(application_id: &str, job_run_id: &str) -> String {
    format!("{application_id}{IN_FLIGHT_KEY_SEPARATOR}{job_run_id}")
}

/// Perform the workload every job run of this store already owes.
///
/// Called by the one clock pump every operation of this service runs before it reads the store
/// ([`super::job_run_logs::publish_due`]) and by `StartJobRun` once it has released the guard:
/// the execution was owed the instant the submit was accepted, whoever asks about it next.
/// Nothing of it happens under the store's guard — the plan is copied out under the guard, the
/// application runs without it, and the outcome is recorded under it again.
///
/// The CALL waits [`WORKLOAD_FIRST_WAIT`] for each application it starts and no longer: one
/// that answers inside that slice decides its run here, and one that does not is LEFT RUNNING,
/// with the run still `RUNNING`, while a TASK of this process carries the rest of the wait and
/// records what the execution then answers ([`perform_handed_off_workload`]) — which is also
/// what makes one submitted application run ONCE however often its run is read. What one call
/// spends is bounded either way ([`MAX_WORKLOAD_RUNS_PER_CALL`] over at most
/// [`WORKLOAD_CALL_CEILING`]).
pub async fn perform_owed<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) {
    perform_owed_waiting(engine, ctx, WORKLOAD_FIRST_WAIT).await;
}

/// START the workload every job run already owes WITHOUT waiting for any of it, which is what
/// the call that ACCEPTS a submit does.
///
/// The work is owed from the instant the submit was accepted, so it is started there — but a
/// submit answers the id it minted rather than waiting on the work, so this waits for nothing:
/// every execution it starts is handed to a task of this process, which records what that
/// execution answers. What the submit is still held by is the one in-process read of the
/// application's own object, which is what decides whether there is anything to run at all.
pub async fn start_owed<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) {
    perform_owed_waiting(engine, ctx, Duration::ZERO).await;
}

/// The one body behind both: `first_wait` is how long this call waits for each application it
/// starts before leaving it in flight.
async fn perform_owed_waiting<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    first_wait: Duration,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    {
        // The executions this process has already finished waiting for are forgotten first,
        // and every state the clock owes this store is materialised: an execution records
        // what it came to as it answers, so a run whose work has finished has its terminal
        // state owed to it from that instant and reaches it here.
        let mut store = state.lock();
        let now = SystemTime::now();
        store.forget_ended_workloads();
        store.settle_all(now);
    }
    let region = request_region(ctx.parts());
    let began = ::std::time::Instant::now();
    let mut started = 0usize;
    while started < MAX_WORKLOAD_RUNS_PER_CALL
        && began
            .elapsed()
            .saturating_add(first_wait)
            .le(&WORKLOAD_CALL_CEILING)
    {
        let claimed = {
            let mut store = state.lock();
            match store.claim_job_run_awaiting_workload() {
                Some(claimed) => claimed,
                None => break,
            }
        };
        let workload = match claimed {
            ClaimedWorkload::Runnable(workload) => workload,
            // A driver nothing here runs is answered at once: there is nothing to start, and
            // the run reports what this emulator really did rather than staying in flight.
            ClaimedWorkload::NotRunnable {
                application_id,
                job_run_id,
                outcome,
            } => {
                record_outcome(&state, &application_id, &job_run_id, &outcome);
                started += 1;
                continue;
            }
        };
        // The credential the run's own reads and writes are made with is minted HERE, where the
        // request's own context is: the session of the run's `executionRoleArn`, which is the
        // identity the application is fetched and executed as.
        let identity = execution_role_identity(
            engine,
            ctx.credential_store(),
            &workload.execution_role_arn,
            JOB_RUN_ROLE_SESSION_NAME,
            SystemTime::now(),
        );
        let access = function_sdk_access(
            engine,
            ctx,
            &region,
            &workload.execution_role_arn,
            JOB_RUN_ROLE_SESSION_NAME,
        );
        perform_workload(
            &state,
            ctx.callback(),
            &region,
            &workload,
            identity,
            access,
            first_wait,
        )
        .await;
        started += 1;
    }
}

/// Record what one execution came to on the run it belongs to.
fn record_outcome(
    state: &::engine::ServiceState<ComAmazonawsEmrserverlessAwstoledowebserviceState>,
    application_id: &str,
    job_run_id: &str,
    outcome: &WorkloadOutcome,
) {
    let mut store = state.lock();
    if let Some(record) = store
        .applications
        .get_mut(application_id)
        .and_then(|application| application.job_runs.get_mut(job_run_id))
    {
        record.record_execution(outcome, SystemTime::now());
    }
}

/// Give the claim on one run's execution back, for an execution nothing performed.
fn release_claim(
    state: &::engine::ServiceState<ComAmazonawsEmrserverlessAwstoledowebserviceState>,
    application_id: &str,
    job_run_id: &str,
) {
    let mut store = state.lock();
    if let Some(record) = store
        .applications
        .get_mut(application_id)
        .and_then(|application| application.job_runs.get_mut(job_run_id))
    {
        record.release_workload_claim();
    }
}

/// Run ONE job run's workload and record what it came to.
///
/// Every answer is grounded in what really happened: the object store's own refusal for an
/// application it does not serve, the exception the application raised, or the bound the
/// interpreter stopped it at. An execution NOTHING performed — no free interpreter, a thread
/// that could not be started — records nothing and gives the claim back, so a later call runs it
/// rather than anything reporting an outcome no execution produced.
async fn perform_workload(
    state: &::engine::ServiceState<ComAmazonawsEmrserverlessAwstoledowebserviceState>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    workload: &JobRunWorkload,
    identity: Option<CallerIdentity>,
    access: Option<AwsAccess>,
    first_wait: Duration,
) {
    let failed = |outcome: WorkloadOutcome| {
        record_outcome(
            state,
            &workload.application_id,
            &workload.job_run_id,
            &outcome,
        );
    };
    let Some((bucket, key)) = ::service_common::s3_uri::object_address(&workload.entry_point)
    else {
        failed(WorkloadOutcome::Failed(job_run_application_unreadable(
            "the entry point does not name an Amazon S3 object",
        )));
        return;
    };
    // The application is FETCHED as the run's own execution role, so the object store authorizes
    // the read against the policies in force on that role and against the bucket's own policy,
    // exactly as it authorizes any other signed request.
    let source = match read_application(callback, region, &bucket, &key, identity).await {
        Ok(source) => source,
        Err(ApplicationNotRead::Denied) => {
            failed(WorkloadOutcome::Failed(job_run_application_not_accessible()));
            return;
        }
        Err(ApplicationNotRead::Unreadable(detail)) => {
            failed(WorkloadOutcome::Failed(job_run_application_unreadable(
                &detail,
            )));
            return;
        }
    };
    // The INTERPRETER permit is taken here and not before the read: it bounds how many
    // applications this process RUNS at once, and a read is not a run — holding one across the
    // read would let a cross-service read nothing answers keep an interpreter from every other
    // job run of this process.
    let Ok(permit) = ::std::sync::Arc::clone(&WORKLOAD_RUN_PERMITS).try_acquire_owned() else {
        ::tracing::debug!(
            application = %workload.application_id,
            job_run = %workload.job_run_id,
            "an EMR Serverless job run found no free interpreter, so it stays owed one"
        );
        release_claim(state, &workload.application_id, &workload.job_run_id);
        return;
    };
    run_application(state, workload, &source, &key, access, permit, first_wait).await;
}

/// Why an application was not read: the two answers that are told apart, because the run words
/// them differently.
#[derive(Debug, Clone)]
enum ApplicationNotRead {
    /// The object store answered that the read is NOT PERMITTED to the run's execution role.
    Denied,
    /// Anything else the read established: the store holds no such object, the object is past
    /// the bound, it is not text, or the store answered something else about it.
    Unreadable(String),
}

/// The application's own source, read from the object the entry point names, as `identity` — the
/// session of the run's execution role, or the emulator's own credential where there is none.
///
/// The read is RANGED, so at most [`MAX_APPLICATION_BYTES`] plus the one byte that shows the
/// object is past the bound is ever buffered.
async fn read_application(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    bucket: &str,
    key: &str,
    identity: Option<CallerIdentity>,
) -> Result<String, ApplicationNotRead> {
    let client = match identity {
        Some(identity) => ::service_common::s3_client::s3_client_signed_as(
            callback,
            region,
            ::aws_sdk_s3::config::Credentials::new(
                identity.access_key_id,
                identity.secret_access_key,
                Some(identity.session_token),
                None,
                JOB_RUN_CREDENTIAL_SOURCE,
            ),
        ),
        None => ::service_common::s3_client::s3_client(callback, region),
    };
    let object = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .range(format!("bytes=0-{MAX_APPLICATION_BYTES}"))
        .send()
        .await
        .map_err(|error| {
            let code = error.code().unwrap_or_default().to_string();
            if ::service_common::s3_object_read::OBJECT_DENIED_CODES.contains(&code.as_str()) {
                return ApplicationNotRead::Denied;
            }
            // What the STORE said about the object, and nothing of the address it was asked
            // about: the entry point is `@sensitive` ([`JOB_RUN_UNREADABLE_LEAD`]).
            ApplicationNotRead::Unreadable(format!(
                "the object store answered {code}: {}",
                error.message().unwrap_or("no further detail")
            ))
        })?;
    let body = object
        .body
        .collect()
        .await
        .map_err(|error| {
            ApplicationNotRead::Unreadable(format!(
                "the application did not arrive in full: {error}"
            ))
        })?
        .into_bytes();
    if body.len() > MAX_APPLICATION_BYTES {
        return Err(ApplicationNotRead::Unreadable(format!(
            "the application is past the {MAX_APPLICATION_BYTES} bytes one may hold"
        )));
    }
    String::from_utf8(body.to_vec())
        .map_err(|_| ApplicationNotRead::Unreadable("the application is not text".to_string()))
}

/// What one run of a submitted application may spend: the run's own
/// `executionTimeoutMinutes` clamped DOWN to this emulator's own ceiling
/// ([`WORKLOAD_RUN_CEILING`]), beside the step, memory, recursion, log and answer ceilings
/// every run here is given.
///
/// The clamp is what keeps a stored timeout — up to the 1 000 000 minutes
/// `Duration`'s `@range` admits — from setting the span one run is given, and a timeout of zero
/// ("Setting a value of 0 to disable the timeout") bounds nothing, so the ceiling binds it.
#[must_use]
pub fn workload_budget(execution_timeout_minutes: i64) -> python::Budget {
    let configured = u64::try_from(execution_timeout_minutes)
        .ok()
        .filter(|minutes| *minutes > 0)
        .map(|minutes| Duration::from_secs(minutes.saturating_mul(60)))
        .unwrap_or(WORKLOAD_RUN_CEILING);
    python::Budget {
        wall_clock: configured.min(WORKLOAD_RUN_CEILING),
        steps: MAX_WORKLOAD_STEPS,
        memory_bytes: MAX_WORKLOAD_MEMORY_BYTES,
        ephemeral_bytes: MAX_WORKLOAD_SCRATCH_BYTES,
        log_bytes: MAX_WORKLOAD_LOG_BYTES,
        response_bytes: MAX_WORKLOAD_ANSWER_BYTES,
        recursion_limit: MAX_WORKLOAD_RECURSION,
    }
}

/// Run `source` as the submitted application, on a thread of its own, and record what it came
/// to on the run it belongs to.
///
/// The interpreter is synchronous and CPU-bound, so it runs on a thread with a stack of its own
/// rather than on the async runtime. The CALLER's wait for it is [`WORKLOAD_FIRST_WAIT`] and no
/// longer: an application that answers inside that slice decides its run on the call that
/// started it, and one that does not is left running while a task of this process carries the
/// rest of the wait ([`WORKLOAD_HOST_WAIT`]) and records what it then answers — so the run stays
/// `RUNNING` until there IS an answer and is never reported as finished for work still in
/// flight.
///
/// `permit` is this run's share of [`MAX_CONCURRENT_WORKLOAD_RUNS`] and is MOVED into that
/// thread, so it is given back when the thread ends — including a thread nobody is waiting for
/// any more.
async fn run_application(
    state: &::engine::ServiceState<ComAmazonawsEmrserverlessAwstoledowebserviceState>,
    workload: &JobRunWorkload,
    source: &str,
    key: &str,
    access: Option<AwsAccess>,
    permit: ::tokio::sync::OwnedSemaphorePermit,
    first_wait: Duration,
) {
    let budget = workload_budget(workload.execution_timeout_minutes);
    // Where the application is reported to run from: the path a Spark submit places its entry
    // point at on the worker that runs it, which is what its own traceback frames name.
    let path = format!("/tmp/{}", key.split('/').next_back().unwrap_or(key));
    let event = ::serde_json::json!({
        "path": path,
        "source": source,
        "arguments": workload.entry_point_arguments,
    })
    .to_string();
    let modules = ::std::iter::once(PackageModule {
        path: format!("{APPLICATION_ENTRY_MODULE}.py"),
        source: APPLICATION_ENTRY_SOURCE.to_string(),
    })
    // The ONE in-process Spark runtime of this workspace, which is what a submitted Spark
    // program resolves its own imports against.
    .chain(
        ::service_common::spark_runtime::pyspark_modules(workload.spark_version.as_deref())
            .into_iter()
            .map(|(path, source)| PackageModule { path, source }),
    )
    .collect::<Vec<_>>();
    let job_run_id = workload.job_run_id.clone();
    let handler = format!("{APPLICATION_ENTRY_MODULE}.{APPLICATION_ENTRY_FUNCTION}");
    let (sender, mut receiver) = ::tokio::sync::oneshot::channel();
    let spawned = ::std::thread::Builder::new()
        .name("emr-serverless-job-run".to_string())
        .stack_size(WORKLOAD_STACK_BYTES)
        .spawn(move || {
            // Held for as long as the application runs, whoever is still waiting for it.
            let _permit = permit;
            let finished = python::run_against(
                &python::Run {
                    modules: &modules,
                    handler: &handler,
                    // A Spark submit's application reads its parameters off its command line, so
                    // the run is given no environment of its own.
                    environment: &::std::collections::BTreeMap::new(),
                    architecture:
                        ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                    event: Some(&event),
                    request_id: &job_run_id,
                    invoked_function_arn: "",
                    client_context: None,
                    tenant_id: None,
                    budget,
                    aws: access.as_ref(),
                    durable: None,
                },
                // The clock the run's own span is read against: one the RUN drives, a tick per
                // traced event it delivers and the waits it asks for, so the state the run comes
                // to rest in is what the application came to rather than what this host's load
                // made of it — stating what the run may take of THIS host besides.
                &python::Clock::driven(WORKLOAD_CLOCK_TICK)
                    .waiting()
                    .within(WORKLOAD_HOST_CEILING),
            );
            let _ = sender.send(finished);
        });
    let application_id = workload.application_id.clone();
    let job_run_id = workload.job_run_id.clone();
    if spawned.is_err() {
        // No thread, so nothing of the application ran: the claim goes back and a later call
        // runs it, rather than this one reporting an outcome no execution produced.
        release_claim(state, &application_id, &job_run_id);
        return;
    }
    match ::tokio::time::timeout(first_wait, &mut receiver).await {
        Ok(Ok(finished)) => {
            record_outcome(
                state,
                &application_id,
                &job_run_id,
                &read_outcome(&finished.completion),
            );
        }
        Ok(Err(_)) => {
            // The thread ended without reporting, so nothing is known about what the
            // application came to.
            record_outcome(
                state,
                &application_id,
                &job_run_id,
                &WorkloadOutcome::Failed(job_run_workload_unanswered()),
            );
        }
        // Still running: the run stays `RUNNING` and the rest of the wait is carried by a TASK
        // of this process, which records what the execution answers AT THE INSTANT it answers
        // ([`perform_handed_off_workload`]). The execution is not given back to the owed queue —
        // its application is really running, and running it a second time would make its
        // effects twice.
        Err(_) => {
            hand_off_execution(state, &application_id, &job_run_id, receiver, first_wait);
        }
    }
}

/// Hand the rest of the wait for one started execution to a TASK of this process, and keep that
/// task's handle on the store.
///
/// What the execution came to is recorded by that task, under the store's guard, at the instant
/// the execution ANSWERED — so the state the clock then owes the run is dated from when its work
/// finished rather than from when somebody next read it, and a run comes to rest whether or not
/// anybody is polling. The task stops waiting after the whole host wait
/// ([`WORKLOAD_HOST_WAIT`], less what the call that started the execution already spent), so a
/// run whose execution never answers reaches the modelled failure state saying so rather than
/// staying `RUNNING`.
fn hand_off_execution(
    state: &::engine::ServiceState<ComAmazonawsEmrserverlessAwstoledowebserviceState>,
    application_id: &str,
    job_run_id: &str,
    answer: ::tokio::sync::oneshot::Receiver<python::Finished>,
    already_waited: Duration,
) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        // No task can carry the wait, so nothing of this process will ever hear the answer:
        // the run says this emulator has no outcome for it rather than staying `RUNNING`.
        record_outcome(
            state,
            application_id,
            job_run_id,
            &WorkloadOutcome::Failed(job_run_workload_unanswered()),
        );
        return;
    };
    let performed = ::engine::ServiceState::clone(state);
    let held_application = application_id.to_string();
    let held_job_run = job_run_id.to_string();
    let task = runtime.spawn(async move {
        perform_handed_off_workload(
            &performed,
            &held_application,
            &held_job_run,
            answer,
            already_waited,
        )
        .await;
    });
    state
        .lock()
        .hold_in_flight_workload(application_id, job_run_id, PendingWorkloadRun { task });
}

/// Wait for one handed-off execution and record what it came to on the run it belongs to.
///
/// The wait is the whole host wait less what the call that started the execution already spent,
/// so this emulator gives up on an execution that has not answered at the same instant whichever
/// call started it; the engine stops the run itself at the host time one run may take
/// ([`WORKLOAD_HOST_CEILING`]), which is what this wait allows for.
async fn perform_handed_off_workload(
    state: &::engine::ServiceState<ComAmazonawsEmrserverlessAwstoledowebserviceState>,
    application_id: &str,
    job_run_id: &str,
    answer: ::tokio::sync::oneshot::Receiver<python::Finished>,
    already_waited: Duration,
) {
    let outcome = match ::tokio::time::timeout(
        WORKLOAD_HOST_WAIT.saturating_sub(already_waited),
        answer,
    )
    .await
    {
        Ok(Ok(finished)) => read_outcome(&finished.completion),
        // The thread ended without reporting, or the whole host wait passed: nothing is known
        // about what the application came to.
        Ok(Err(_)) | Err(_) => WorkloadOutcome::Failed(job_run_workload_unanswered()),
    };
    record_outcome(state, application_id, job_run_id, &outcome);
}

/// What the run reports for an application the interpreter answered for.
fn read_outcome(completion: &python::Completion) -> WorkloadOutcome {
    match completion {
        // A submitted application is a program rather than a function, so what it ANSWERED is
        // not part of the run: an application that ran to its end exited cleanly.
        python::Completion::Returned(_) | python::Completion::Streamed { .. } => {
            WorkloadOutcome::Succeeded
        }
        python::Completion::Raised {
            error_type,
            error_message,
            ..
        } => WorkloadOutcome::Failed(job_run_application_failed(error_type, error_message)),
        python::Completion::Stopped(python::Stop::Deadline) => WorkloadOutcome::Failed(
            job_run_bound_reached("the span this emulator runs one submitted application for"),
        ),
        python::Completion::Stopped(python::Stop::Steps) => WorkloadOutcome::Failed(
            job_run_bound_reached("the step budget this emulator gives one application"),
        ),
        python::Completion::Stopped(python::Stop::Memory) => WorkloadOutcome::Failed(
            job_run_bound_reached("the memory ceiling this emulator gives one application"),
        ),
        python::Completion::Stopped(python::Stop::Response) => WorkloadOutcome::Failed(
            job_run_bound_reached("the size of the value one application may answer with"),
        ),
        // Not a bound of the application's own run: the host time one run may take is this
        // emulator giving up on the execution, so the run says this emulator has no outcome for
        // it rather than that the application was stopped for taking too long.
        python::Completion::Stopped(python::Stop::HostCeiling) => {
            WorkloadOutcome::Failed(job_run_workload_unanswered())
        }
        python::Completion::Internal(detail) => {
            WorkloadOutcome::Failed(job_run_application_failed("InternalError", detail))
        }
    }
}

/// The module this emulator runs the submitted application FROM, and the function in it the
/// engine calls.
const APPLICATION_ENTRY_MODULE: &str = "__emr_serverless_job_run";
/// See [`APPLICATION_ENTRY_MODULE`].
const APPLICATION_ENTRY_FUNCTION: &str = "run";

/// The Python source of that module: it runs the submitted application the way a Python program
/// is run — as the `__main__` module, with the submit's `entryPointArguments` on `sys.argv` —
/// and lets whatever the application raises travel out to the engine, which is what reports it.
///
/// An application is a PROGRAM, so the way it ENDS is its exit status: `sys.exit()` raises
/// `SystemExit`, whose `code` is `None` for a plain `sys.exit()` and the status for
/// `sys.exit(status)` — and Python's own rule is that `None` is equivalent to zero and any other
/// object means a failure. So an application that exits with a status of zero (or with none at
/// all) ran to its end and the run SUCCEEDS, while one that exits with any other status failed
/// and that status travels on as the program's own, exactly as a non-zero exit of a Spark
/// submit's entry point fails its job run.
///
/// Nothing here is a bound or a containment: both are the engine's, in host code no application
/// can reach.
const APPLICATION_ENTRY_SOURCE: &str = r#"
"""Runs the application a Spark submit named, as a Python program with its arguments."""

import sys


def run(event, context):
    # The command line a Spark submit's entry point reads its arguments off: the
    # application's own path followed by the submit's `entryPointArguments`.
    sys.argv = [event["path"]] + list(event["arguments"])
    # An application is run as a program, so its globals are those of the main module and its
    # `if __name__ == "__main__":` guard holds, exactly as on an EMR Serverless worker.
    program_globals = {"__name__": "__main__", "__file__": event["path"]}
    try:
        exec(compile(event["source"], event["path"], "exec"), program_globals)
    except SystemExit as exiting:
        # How a program ENDS: `sys.exit()` with no status, or with zero, is an application that
        # ran to its end. Any other status is the application failing, and it travels on to the
        # engine as the program's own exit status.
        if exiting.code is not None and exiting.code != 0:
            raise
    return None
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the state a run comes to rest in is what its execution answered — `SUCCESS` only
    /// for an application that ran to its end, and the modelled failure state for everything
    /// else, carrying what really happened.
    ///
    /// One row per completion the engine has, so a bound added to the engine has to be answered
    /// here too.
    #[test]
    fn the_state_a_run_rests_in_is_what_its_execution_answered() {
        // (case, what the engine answered, the state the run rests in, what its details say)
        let cases: Vec<(&str, python::Completion, &str, &str)> = vec![
            (
                "an application that ran to its end",
                python::Completion::Returned("null".to_string()),
                "SUCCESS",
                SUCCESS_STATE_DETAILS,
            ),
            (
                "an application that raised",
                python::Completion::Raised {
                    error_type: "ValueError".to_string(),
                    error_message: "the input partition is malformed".to_string(),
                    stack_trace: Vec::new(),
                    initialization: false,
                },
                "FAILED",
                "ValueError: the input partition is malformed",
            ),
            (
                "an application that ran past the span one run may last",
                python::Completion::Stopped(python::Stop::Deadline),
                "FAILED",
                "the span this emulator runs one submitted application for",
            ),
            (
                "an application that spent its step budget",
                python::Completion::Stopped(python::Stop::Steps),
                "FAILED",
                "the step budget this emulator gives one application",
            ),
            (
                "an application that spent its memory ceiling",
                python::Completion::Stopped(python::Stop::Memory),
                "FAILED",
                "the memory ceiling this emulator gives one application",
            ),
            (
                "an application that answered with more than one may",
                python::Completion::Stopped(python::Stop::Response),
                "FAILED",
                "the size of the value one application may answer with",
            ),
            (
                "a run this emulator gave up on at the host time one run may take",
                python::Completion::Stopped(python::Stop::HostCeiling),
                "FAILED",
                "no outcome to report for the job run",
            ),
            (
                "an interpreter that could not be driven at all",
                python::Completion::Internal("the interpreter could not be built".to_string()),
                "FAILED",
                "the interpreter could not be built",
            ),
        ];
        for (case, completion, state, says) in cases {
            let outcome = read_outcome(&completion);
            assert_eq!(
                outcome.job_run_state(),
                state,
                "case {case:?}: expected the run to rest in {state}, got {outcome:?}"
            );
            let details = outcome.state_details();
            assert!(
                details.contains(says),
                "case {case:?}: expected the details to say {says:?}, got {details:?}"
            );
            if outcome == WorkloadOutcome::Succeeded {
                // RECORDED behaviour: a run that ran to its end carries NO details, which is
                // what the service sends for one ([`SUCCESS_STATE_DETAILS`]).
                assert_eq!(
                    details, SUCCESS_STATE_DETAILS,
                    "case {case:?}: expected a successful run to carry no details, got \
                     {details:?}"
                );
                continue;
            }
            // Every outcome that says what happened says it inside `String256`'s modelled
            // length.
            assert!(
                (1..=STATE_DETAILS_MAX_CHARS).contains(&details.chars().count()),
                "case {case:?}: stateDetails is String256, got {} characters: {details:?}",
                details.chars().count()
            );
        }
    }

    /// RULE: `stateDetails` stays inside `String256`'s modelled length however long the reason
    /// is, and is never blank (its `@pattern(".*\\S.*")`).
    #[test]
    fn state_details_stay_inside_the_modelled_length() {
        // (case, the reason the execution gave)
        let rows = [
            ("a short reason", "ValueError: no".to_string()),
            (
                "a reason at the modelled ceiling",
                "e".repeat(STATE_DETAILS_MAX_CHARS),
            ),
            (
                "a reason one character past it",
                "e".repeat(STATE_DETAILS_MAX_CHARS + 1),
            ),
            (
                "a reason far past it",
                "e".repeat(STATE_DETAILS_MAX_CHARS * 7),
            ),
            (
                "a reason past it written in multi-byte characters",
                "é".repeat(STATE_DETAILS_MAX_CHARS + 3),
            ),
            ("a reason that is only whitespace", "   ".to_string()),
        ];
        for (case, reason) in rows {
            let details = WorkloadOutcome::Failed(reason.clone()).state_details();
            let characters = details.chars().count();
            assert!(
                (1..=STATE_DETAILS_MAX_CHARS).contains(&characters),
                "case {case:?}: expected 1..={STATE_DETAILS_MAX_CHARS} characters, got \
                 {characters}: {details:?}"
            );
            assert!(
                details.trim() == details && !details.trim().is_empty(),
                "case {case:?}: stateDetails must be non-blank and untrimmed, got {details:?}"
            );
        }
    }

    /// RULE: the span one run may last is the caller's own `executionTimeoutMinutes` held down
    /// to this emulator's ceiling, and a timeout that bounds nothing leaves the ceiling binding.
    #[test]
    fn the_run_span_is_the_configured_timeout_held_to_the_ceiling() {
        // (case, executionTimeoutMinutes, the span one run is given)
        let rows: [(&str, i64, Duration); 6] = [
            ("the range's minimum, which disables the timeout", 0, WORKLOAD_RUN_CEILING),
            ("a timeout under the ceiling", 1, Duration::from_secs(60).min(WORKLOAD_RUN_CEILING)),
            ("the documented default", 720, WORKLOAD_RUN_CEILING),
            ("the range's maximum", 1_000_000, WORKLOAD_RUN_CEILING),
            ("a value outside the modelled range", -5, WORKLOAD_RUN_CEILING),
            ("a value that would overflow a span", i64::MAX, WORKLOAD_RUN_CEILING),
        ];
        for (case, minutes, expected) in rows {
            let actual = workload_budget(minutes).wall_clock;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected a span of {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the Spark version an application is told it runs on is the one its own Amazon EMR
    /// release ships, read from the Amazon EMR release catalog; a label that catalog knows
    /// nothing about tells it nothing.
    ///
    /// SOURCE: the Amazon EMR release catalog this workspace keeps for
    /// `DescribeReleaseLabel`, which is what every release's application versions come from.
    #[test]
    fn the_spark_version_is_the_one_the_releases_catalog_ships() {
        // (case, the release label, whether a version is known for it)
        let rows = [
            ("a release EMR Serverless serves", "emr-7.1.0", true),
            ("an older release of the same line", "emr-6.15.0", true),
            ("the earliest release EMR Serverless is offered from", "emr-6.6.0", true),
            ("a label naming no release at all", "emr-not-a-release", false),
            ("no label at all", "", false),
        ];
        for (case, label, known) in rows {
            let actual = spark_version_of(label);
            assert_eq!(
                actual.is_some(),
                known,
                "case {case:?}: expected a known version = {known}, got {actual:?}"
            );
            if let Some(version) = actual {
                assert!(
                    version.split('.').count() >= 2
                        && version.chars().all(|c| c.is_ascii_digit() || c == '.'),
                    "case {case:?}: expected a dotted Spark version, got {version:?}"
                );
            }
        }
    }

    /// RULE: the engine ANSWERS for the budget, the clock and the STACK this service gives one
    /// run, so a submitted application that ends decides its run rather than leaving it in
    /// flight.
    ///
    /// The rows span what an application can do with the one thing the entry module gives it —
    /// run to its end, end the program cleanly, end it with a status, and raise — because the
    /// state a run comes to rest in is read off exactly that answer ([`read_outcome`]). Each is
    /// run on a thread with the stack a run is really given ([`WORKLOAD_STACK_BYTES`]), because
    /// the engine's own import chain is what that stack is sized for.
    #[test]
    fn the_engine_answers_for_the_budget_one_run_is_given() {
        // (case, the application's own source, the state the run comes to rest in)
        let rows: [(&str, &str, &str); 4] = [
            ("an application that ran to its end", "value = 1 + 1\n", "SUCCESS"),
            (
                "an application that ends the program cleanly",
                "import sys\nsys.exit(0)\n",
                "SUCCESS",
            ),
            (
                "an application that ends the program with a status",
                "import sys\nsys.exit(3)\n",
                "FAILED",
            ),
            (
                "an application that raises",
                "raise ValueError('no partition')\n",
                "FAILED",
            ),
        ];
        for (case, source, state) in rows {
            let event = ::serde_json::json!({
                "path": "/tmp/main.py",
                "source": source,
                "arguments": Vec::<String>::new(),
            })
            .to_string();
            let finished = ::std::thread::Builder::new()
                .stack_size(WORKLOAD_STACK_BYTES)
                .spawn(move || {
                    let modules = vec![PackageModule {
                        path: format!("{APPLICATION_ENTRY_MODULE}.py"),
                        source: APPLICATION_ENTRY_SOURCE.to_string(),
                    }];
                    let handler =
                        format!("{APPLICATION_ENTRY_MODULE}.{APPLICATION_ENTRY_FUNCTION}");
                    python::run_against(
                        &python::Run {
                            modules: &modules,
                            handler: &handler,
                            environment: &::std::collections::BTreeMap::new(),
                            architecture:
                                ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                            event: Some(&event),
                            request_id: "0000000000000001",
                            invoked_function_arn: "",
                            client_context: None,
                            tenant_id: None,
                            budget: workload_budget(DEFAULT_EXECUTION_TIMEOUT_MINUTES),
                            // No seam: what this row establishes is the ENGINE's answer, and
                            // an application making no call needs none.
                            aws: None,
                            durable: None,
                        },
                        &python::Clock::driven(WORKLOAD_CLOCK_TICK)
                            .waiting()
                            .within(WORKLOAD_HOST_CEILING),
                    )
                })
                .expect("the thread one run is given can be started")
                .join()
                .expect("the thread one run is given answers");
            let outcome = read_outcome(&finished.completion);
            assert_eq!(
                outcome.job_run_state(),
                state,
                "case {case:?}: expected the run to rest in {state}, got {outcome:?} from \
                 {:?}",
                finished.completion
            );
        }
    }
}
