// SPDX-License-Identifier: CC0-1.0

//! RUNNING the code block one calculation submitted, and what the calculation then reports.
//!
//! `StartCalculationExecution` submits "a string that contains the code of the calculation"
//! into a session, and what a later read reports — the `stdout` of the calculation, its
//! `stderr`, whether it completed or failed — is whatever that code produced. So the code is
//! RUN, and neither the engine that runs it nor the containment around it is stood up here:
//!
//! * it is EXECUTED by the contained Python interpreter this workspace already embeds (the
//!   Lambda service crate's [`python`]), with the Spark runtime the workspace supplies to
//!   every service whose data plane runs a caller-supplied Spark program
//!   ([`::service_common::spark_runtime`]) on its import path — so `pyspark` resolves for a
//!   calculation here exactly as it does for a submitted application there, and the way a
//!   program is run and reported is the same in both;
//! * what the CALCULATION reports is what that run answered: a code block that ran to its end
//!   COMPLETED with the output it wrote, and one that raised FAILED carrying its own traceback
//!   on the error stream. A run a bound of THIS EMULATOR cut short, or one nothing answered
//!   for, is neither ([`Completion::stopped`]): nothing is known about what the code block
//!   came to, so no outcome is claimed for it;
//! * and the code block is a cell of the session it was submitted into, so it finds that
//!   session's own engine already bound — `spark`, and `sc` for its context — exactly as a
//!   cell of an interactive Spark session does
//!   ([`::service_common::spark_runtime::SESSION_BOUND_EVENT_MEMBER`]).
//!
//! # Bounds
//!
//! A code block is caller-supplied code, so every bound is this emulator's and none of them is
//! reachable from the code: how many calculations this process runs at once
//! ([`MAX_CONCURRENT_CALCULATION_RUNS`], with a bounded wait for a permit), the span of the
//! RUN's own time it may last and the step, memory, recursion and output ceilings the
//! interpreter is given ([`calculation_budget`]), and the HOST time one run may take beside
//! them ([`CALCULATION_HOST_CEILING`]). The containment — the native modules that are never
//! registered, the host surface that is compiled out and the stop no guest call can clear — is
//! that engine's, which is why nothing of it is stood up again here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;

/// The longest ONE run of a submitted code block may last, in the RUN's own time.
///
/// CHOSEN, not an AWS duration: a real calculation runs for as long as its session gives it,
/// so this is the span one gets here, and a code block that runs past it fails the calculation
/// on that bound rather than being left running. It is a span of the RUN's own time rather
/// than of this host's — spent by the waits the code asks for and by one tick per traced event
/// it delivers ([`CALCULATION_CLOCK_TICK`]) — so what a calculation comes to rest in is the
/// same on an idle host and on a loaded one.
pub const CALCULATION_RUN_CEILING: Duration = Duration::from_secs(5);

/// How far ONE reading of a calculation run's clock moves it: sized so the whole step budget
/// ([`MAX_CALCULATION_STEPS`]) spends well under the span above, which leaves a code block
/// that computes without end to be answered by that budget — a count of its OWN work — and
/// leaves the span itself for the waits a code block asks for.
pub const CALCULATION_CLOCK_TICK: Duration = Duration::from_nanos(2_500);

/// How much of THIS HOST's own time ONE run may take, past which the engine stops it whatever
/// its own clock reads.
///
/// CHOSEN: the span above is the RUN's own time, so it bounds no CPU of this host. This is the
/// bound that does: the engine reads it from this host's own clock at every traced event and
/// from the host's own watch over the run ([`python::Clock::within`]), so a run of
/// caller-supplied code is bounded on this host however its own time is read. It is generous
/// beside the spans the engine's own phases take, because a calculation that FINISHED must be
/// reported on what it came to rather than on how fast this host read it.
pub const CALCULATION_HOST_CEILING: Duration =
    ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::INIT_PHASE_LIMIT
        .saturating_add(CALCULATION_RUN_CEILING)
        .saturating_mul(HOST_CEILING_OVER_THE_ENGINE_SPAN);

/// How many times the engine's own spans the host ceiling allows for
/// ([`CALCULATION_HOST_CEILING`]).
const HOST_CEILING_OVER_THE_ENGINE_SPAN: u32 = 2;

/// How long past the host time one run may take this emulator still waits for its thread: the
/// engine's stop is latched by the host and raised at the next traced event the interpreter
/// delivers, so a thread it has stopped answers a moment after the ceiling rather than at it.
const CALCULATION_ABANDON_GRACE: Duration = Duration::from_secs(5);

/// How many calculations this process runs AT ONCE.
///
/// CHOSEN: a run is an interpreter, a thread and the memory the two hold, so the number of
/// them this process carries is bounded rather than following from how many calculations a
/// caller submits. The permit is held by the run's own thread and given back when that thread
/// ends — including a run nobody is waiting for any more.
pub const MAX_CONCURRENT_CALCULATION_RUNS: usize = 4;

/// How long a submission waits for one of those permits before nothing is run for it.
///
/// CHOSEN: long enough that a submission arriving while the permits are taken is RUN rather
/// than refused — one run ahead of it cannot last longer than the host ceiling and its grace —
/// and bounded, so a flood of submissions holds no request for ever.
pub const CALCULATION_PERMIT_WAIT: Duration = Duration::from_secs(30);

/// The stack a calculation's thread is given: well above the interpreter's clamped recursion
/// limit, so deep recursion ends the run with the interpreter's own `RecursionError` rather
/// than touching the thread's guard page.
const CALCULATION_STACK_BYTES: usize = 16 * 1024 * 1024;

/// How many traced events one calculation may execute.
///
/// CHOSEN: large enough for the transformations one notebook cell computes, and small enough
/// that a code block which computes without end is answered inside the span the SUBMISSION
/// waits for it — the budget and the run's own ceiling are sized to be spent together
/// ([`CALCULATION_CLOCK_TICK`]), so a runaway cell is stopped by a count of its OWN work
/// rather than by how fast this host reads it.
const MAX_CALCULATION_STEPS: u64 = 2_000_000;

/// How much memory one calculation may add to this process. CHOSEN, for the same reason.
const MAX_CALCULATION_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// How much one calculation may write to the `/tmp` of the environment it runs in. CHOSEN: that
/// directory is a disk on a real session and is held in this process's memory here, so what one
/// run leaves there is bounded like everything else it holds.
const MAX_CALCULATION_SCRATCH_BYTES: u64 = 256 * 1024 * 1024;

/// How deep one calculation may recurse. The interpreter's own default is below what the frozen
/// standard library's import chain needs, so this is the limit a run is given.
const MAX_CALCULATION_RECURSION: usize = 2_000;

/// How much of what the interpreter's own runtime writes is kept, and how large a value the
/// run may answer with: the two bounded streams travel back in the document the run answers
/// with, which is what the second ceiling admits.
const MAX_CALCULATION_LOG_BYTES: usize = 64 * 1024;
/// See [`MAX_CALCULATION_LOG_BYTES`].
const MAX_CALCULATION_ANSWER_BYTES: usize = 1024 * 1024;

/// How many characters of EACH console stream one calculation keeps, which is also the most
/// either of its two output objects can hold.
pub const CALCULATION_STREAM_CHARACTERS: usize = 64 * 1024;

/// The path a calculation's code block is run as, which is what its traceback names and what
/// the run puts on `sys.argv`: a notebook cell has no file of its own, so it is named for what
/// it is.
const CALCULATION_PATH: &str = "calculation.py";

/// The permits [`MAX_CONCURRENT_CALCULATION_RUNS`] hands out, for the whole process.
///
/// Held as an owned permit MOVED INTO the thread that runs the calculation, so it is given back
/// when that thread really ends — including a run the caller has already given up waiting for,
/// which would otherwise take this bound past its count while its thread was still going.
static CALCULATION_RUN_PERMITS: ::std::sync::LazyLock<Arc<::tokio::sync::Semaphore>> =
    ::std::sync::LazyLock::new(|| {
        Arc::new(::tokio::sync::Semaphore::new(
            MAX_CONCURRENT_CALCULATION_RUNS,
        ))
    });

/// How long the SUBMISSION that performs a run may be held by it: the wait for a permit, the
/// host time one run may take, and the grace a stopped thread answers inside.
///
/// Stated here so the operation that hands a request's own code to this engine can bound the
/// call itself ([`not_answered`]): every bound below is the engine's, and this is the one the
/// HANDLER applies, so no submission holds a request past it whatever the code block does.
pub const CALCULATION_CALL_CEILING: Duration = CALCULATION_PERMIT_WAIT
    .saturating_add(CALCULATION_HOST_CEILING)
    .saturating_add(CALCULATION_ABANDON_GRACE);

/// What a calculation reports when the submission's own ceiling
/// ([`CALCULATION_CALL_CEILING`]) passed before the run answered: the same failure a run
/// nothing answered for reports, because that is what happened.
#[must_use]
pub fn not_answered() -> Completion {
    not_run(NO_INTERPRETER, CALCULATION_CALL_CEILING)
}

/// What ONE run of a calculation produced, as the host read it back off the run.
///
/// Every field is a READING of the run: the two streams are what the code block wrote, and the
/// duration is how long the host spent on it. Nothing here is derived from the submitted text.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    /// What the calculation wrote to its standard output.
    pub stdout: String,
    /// What it wrote to its standard error, including the traceback it ended on and the bound
    /// that stopped it — each as the stream a real run reports them on.
    pub stderr: String,
    /// Whether the run FAILED: the code raised or exited non-zero, a bound this emulator holds
    /// a run to stopped it, or nothing answered for it at all.
    pub failed: bool,
    /// Whether the run was CUT SHORT by this emulator rather than by anything the code did: a
    /// bound of this emulator stopped it, or nothing answered for it at all.
    ///
    /// Such a run says nothing about what the code block came to — the cell is still work in
    /// flight, which is the state a real session leaves one in until it finishes or something
    /// cancels it — so the submission records no outcome from it
    /// (`crate::ops::start_calculation_execution`), rather than reporting this emulator's own
    /// stop as the cell's failure.
    pub stopped: bool,
    /// How long the host spent running it.
    pub run_time: Duration,
}

/// What one run of a calculation may spend, as the engine takes it.
#[must_use]
pub fn calculation_budget() -> python::Budget {
    python::Budget {
        wall_clock: CALCULATION_RUN_CEILING,
        steps: MAX_CALCULATION_STEPS,
        memory_bytes: MAX_CALCULATION_MEMORY_BYTES,
        ephemeral_bytes: MAX_CALCULATION_SCRATCH_BYTES,
        log_bytes: MAX_CALCULATION_LOG_BYTES,
        response_bytes: MAX_CALCULATION_ANSWER_BYTES,
        recursion_limit: MAX_CALCULATION_RECURSION,
    }
}

/// Run `code` as the one calculation `calculation_execution_id` is, and answer what it
/// produced.
///
/// The interpreter is synchronous and CPU-bound, so the run happens on a thread with a stack of
/// its own rather than on the async runtime, and no store of this service is held across it. A
/// run nothing answered for — no permit inside the bounded wait, a thread that could not be
/// started, or one still going past the host ceiling and its grace — is reported as a
/// calculation that FAILED on this emulator rather than as one that produced something: the
/// permit and the thread are the engine's to release, and its own bounds are unaffected.
pub async fn run(code: String, calculation_execution_id: String) -> Completion {
    let started = Instant::now();
    let Ok(permit) = ::tokio::time::timeout(
        CALCULATION_PERMIT_WAIT,
        Arc::clone(&CALCULATION_RUN_PERMITS).acquire_owned(),
    )
    .await
    .map(|acquired| acquired.ok())
    else {
        return not_run(NO_INTERPRETER, started.elapsed());
    };
    let Some(permit) = permit else {
        return not_run(NO_INTERPRETER, started.elapsed());
    };
    let mut event = ::serde_json::json!({
        "path": CALCULATION_PATH,
        "source": code,
        "arguments": Vec::<String>::new(),
        "streamCharacters": CALCULATION_STREAM_CHARACTERS,
    });
    // The code block is a CELL of a session that is already running, so the session's own
    // engine is bound for it before its first line — the environment an interactive Spark
    // session gives a cell, rather than the empty one a standalone submit starts in.
    event[::service_common::spark_runtime::SESSION_BOUND_EVENT_MEMBER] =
        ::serde_json::Value::Bool(true);
    let event = event.to_string();
    let (sender, receiver) = ::tokio::sync::oneshot::channel();
    let spawned = ::std::thread::Builder::new()
        .name("athena-calculation".to_string())
        .stack_size(CALCULATION_STACK_BYTES)
        .spawn(move || {
            // Held for as long as the calculation runs, whoever is still waiting for it.
            let _permit = permit;
            let modules: Vec<PackageModule> =
                ::service_common::spark_runtime::submitted_program_modules(None)
                    .into_iter()
                    .map(|(path, source)| PackageModule { path, source })
                    .collect();
            let handler = ::service_common::spark_runtime::submitted_program_handler();
            let finished = python::run_against(
                &python::Run {
                    modules: &modules,
                    handler: &handler,
                    // A calculation reads no environment of its own: what it is given is the
                    // code block and nothing besides.
                    environment: &BTreeMap::new(),
                    architecture:
                        ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                    event: Some(&event),
                    request_id: &calculation_execution_id,
                    invoked_function_arn: "",
                    client_context: None,
                    tenant_id: None,
                    budget: calculation_budget(),
                    // A calculation makes no AWS SDK call through the engine's seam: what it
                    // reads and writes of this emulator is what its own session's results
                    // location holds, written by this service.
                    aws: None,
                    // A calculation is no Lambda durable execution's run, so it takes no step
                    // of one and checkpoints nothing onto one.
                    durable: None,
                    // And it mounts no Amazon EFS file system: what the code reads and writes
                    // is the session's own results location, which this service holds.
                    file_systems: &[],
                },
                // The clock the run's own span is read against: one the RUN drives, a tick per
                // traced event it delivers and the waits it asks for, so what the CALCULATION
                // comes to rest in is what the code came to rather than what this host's load
                // made of it — stating what the run may take of THIS host besides, which is
                // the bound the engine stops it on whatever its own clock reads.
                &python::Clock::driven(CALCULATION_CLOCK_TICK)
                    .waiting()
                    .within(CALCULATION_HOST_CEILING),
            );
            let _ = sender.send(finished);
        });
    if spawned.is_err() {
        return not_run(NO_INTERPRETER, started.elapsed());
    }
    // The caller's wait is bounded beside the run's own: a thread still going past it is left
    // to the engine's permit, and the calculation is reported as one this emulator stopped.
    let waited = CALCULATION_HOST_CEILING.saturating_add(CALCULATION_ABANDON_GRACE);
    match ::tokio::time::timeout(waited, receiver).await {
        Ok(Ok(finished)) => read_run(&finished.completion, started.elapsed()),
        // The thread ended without reporting, or is still going: nothing answered for the code
        // block, which is this emulator's own failure rather than anything the code did.
        _ => not_run(NO_INTERPRETER, started.elapsed()),
    }
}

/// What a calculation reports when nothing ran it at all. Fixed text of this emulator's, on the
/// stream a run's own failure is reported on: it says what did not happen rather than inventing
/// an outcome for it.
const NO_INTERPRETER: &str =
    "this emulator did not run the calculation: no interpreter answered for it";

/// The completion of a run that was not performed, carrying `reason` on its error stream.
fn not_run(reason: &str, run_time: Duration) -> Completion {
    Completion {
        stdout: String::new(),
        stderr: format!("{reason}\n"),
        failed: true,
        // Nothing answered for the code block, so what it came to is not known.
        stopped: true,
        run_time,
    }
}

/// What the calculation produced, given what the interpreter answered.
fn read_run(completion: &python::Completion, run_time: Duration) -> Completion {
    match completion {
        // The entry module REPORTS what the run came to
        // ([`::service_common::spark_runtime`]): the status the program exited with, the two
        // console streams it wrote, and the exception it ended on when it ended on one.
        python::Completion::Returned(answered) => reported_run(answered, run_time),
        // A streaming answer is one nothing here asks for, so a run that produced one is a run
        // this service can say nothing about.
        python::Completion::Streamed { .. } => not_run(NO_INTERPRETER, run_time),
        // The entry module itself raised, which is not the code block's own failure: the
        // engine's rendering of the exception says why, and what the code block came to is not
        // known.
        python::Completion::Raised {
            error_type,
            error_message,
            ..
        } => Completion {
            stdout: String::new(),
            stderr: format!("{error_type}: {error_message}\n"),
            failed: true,
            stopped: true,
            run_time,
        },
        // A BOUND of this emulator stopped the run: the code block was still going, so the
        // bound is named on the error stream and the run is marked as the one this emulator cut
        // short rather than as the code block's own failure.
        python::Completion::Stopped(stop) => Completion {
            stdout: String::new(),
            stderr: format!("{}\n", stop.reason()),
            failed: true,
            stopped: true,
            run_time,
        },
        // The interpreter could not be built or driven, which says nothing about the code.
        python::Completion::Internal(_) => not_run(NO_INTERPRETER, run_time),
    }
}

/// The run as the entry module reported it: the two streams it kept, and whether the program
/// ended cleanly.
///
/// A document this emulator's own entry module wrote, so a document that does not parse is this
/// emulator's failure rather than the code block's.
fn reported_run(answered: &str, run_time: Duration) -> Completion {
    let Ok(reported) = ::serde_json::from_str::<::serde_json::Value>(answered) else {
        return not_run(NO_INTERPRETER, run_time);
    };
    let text = |member: &str| {
        reported
            .get(member)
            .and_then(::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let status = reported
        .get("exitStatus")
        .and_then(::serde_json::Value::as_i64)
        .unwrap_or_default();
    Completion {
        stdout: text("stdout"),
        stderr: text("stderr"),
        // "For a step to be considered complete, the main function must exit with a zero exit
        // code": a program that exited non-zero, or that raised and so exited non-zero, is a
        // calculation that failed.
        failed: status != 0,
        // The run ANSWERED, whatever it came to.
        stopped: false,
        run_time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `code` as one calculation, on the current test's runtime.
    async fn ran(code: &str) -> Completion {
        run(code.to_string(), "calculation-under-test".to_string()).await
    }

    /// RULE: the answer to a calculation is what its code COMPUTED, not anything read off the
    /// submitted text — so the rows below are expressions whose value no part of this emulator
    /// holds, and one of them resolves the Spark package this runtime supplies.
    ///
    /// One test over a table rather than a test per row: every row stands an interpreter up,
    /// so what the rows share is paid once.
    #[tokio::test]
    async fn a_calculation_reports_what_its_code_computed() {
        // (case, the code block, what its standard output must hold)
        let rows: &[(&str, &str, &str)] = &[
            ("one sum", "print(5 + 6)", "11\n"),
            (
                "a loop the text does not carry the answer of",
                "total = 0\nfor index in range(1, 101):\n    total += index\nprint(total)\n",
                "5050\n",
            ),
            (
                "the standard library's own json",
                "import json\nprint(json.dumps({'a': [1, 2]}, separators=(',', ':')))",
                "{\"a\":[1,2]}\n",
            ),
            (
                "text outside ASCII",
                "print('héllo wörld ✓'.upper())",
                "HÉLLO WÖRLD ✓\n",
            ),
            ("no output at all", "x = 1 + 1", ""),
            (
                "the Spark configuration the runtime supplies",
                "from pyspark import SparkConf\nprint(SparkConf().setAppName('a').get('spark.app.name'))",
                "a\n",
            ),
            (
                // The session's own engine, bound for the cell the way an interactive Spark
                // session binds it: the sum is COMPUTED over the span, and no part of this
                // emulator holds it.
                "the session bound as `spark`",
                "print(spark.range(1, 101).groupBy().sum().collect()[0][0])",
                "5050\n",
            ),
            (
                "the session's context bound as `sc`",
                "print(sc.parallelize(range(1, 11)).map(lambda n: n * n).sum())",
                "385\n",
            ),
        ];
        for (case, code, expected) in rows {
            let completion = ran(code).await;
            assert_eq!(
                completion.stdout, *expected,
                "case {case:?}: expected the computed output {expected:?}, got {completion:?}"
            );
            assert!(
                !completion.failed,
                "case {case:?}: the calculation must not fail, got {completion:?}"
            );
        }
    }

    /// RULE: a calculation whose code RAISES failed, and what it reports is the traceback the
    /// run wrote on its error stream — the code's own failure, not a run this emulator cut
    /// short.
    #[tokio::test]
    async fn a_calculation_that_raises_reports_the_exception() {
        let rows: &[(&str, &str, &str)] = &[
            ("a name that is not bound", "print(missing)", "NameError"),
            ("a division by zero", "print(1 / 0)", "ZeroDivisionError"),
            ("code that does not parse", "print('unclosed", "SyntaxError"),
            ("an exception of its own", "raise ValueError('no')", "ValueError"),
        ];
        for (case, code, expected) in rows {
            let completion = ran(code).await;
            assert!(
                completion.failed,
                "case {case:?}: the calculation must fail, got {completion:?}"
            );
            assert!(
                !completion.stopped,
                "case {case:?}: the code's own failure is not a run cut short, got {completion:?}"
            );
            assert!(
                completion.stderr.contains(expected),
                "case {case:?}: the failure must be on the error stream, got {completion:?}"
            );
        }
    }

    /// RULE: the bounds a run is held to are the host's, so a calculation that will not stop is
    /// stopped — including one that catches every exception the stop is delivered as — and what
    /// it reports is that this emulator cut the run short, never an outcome.
    #[tokio::test]
    async fn a_calculation_cannot_outlast_the_bounds_the_host_holds_it_to() {
        let rows: &[(&str, &str)] = &[
            ("a tight loop", "while True:\n    pass\nprint('escaped')\n"),
            (
                "a tight loop that catches every stop",
                "while True:\n    try:\n        pass\n    except BaseException:\n        pass\n",
            ),
            ("a wait past the span one run may last", "import time\ntime.sleep(600)"),
        ];
        for (case, code) in rows {
            let completion = ran(code).await;
            assert!(
                completion.failed,
                "case {case:?}: a stopped calculation has failed, got {completion:?}"
            );
            assert!(
                completion.stopped,
                "case {case:?}: a run this emulator cut short must say so, got {completion:?}"
            );
            assert!(
                !completion.stdout.contains("escaped"),
                "case {case:?}: nothing past the stop may be reported, got {completion:?}"
            );
        }
    }

    /// RULE: what one calculation keeps of a stream is bounded, so a code block that writes
    /// without limit cannot grow this process through its own output.
    #[tokio::test]
    async fn the_output_one_calculation_produces_is_capped() {
        let completion = ran("print('x' * 1000000)").await;
        assert!(
            completion.stdout.chars().count() <= CALCULATION_STREAM_CHARACTERS,
            "the kept output must be capped at {CALCULATION_STREAM_CHARACTERS} characters, got \
             {} characters",
            completion.stdout.chars().count()
        );
    }
}
