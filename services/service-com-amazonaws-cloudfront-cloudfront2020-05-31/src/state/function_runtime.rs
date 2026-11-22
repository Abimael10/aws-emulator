// SPDX-License-Identifier: CC0-1.0

//! RUNNING a CloudFront function: the engine the stored function code executes in, inside
//! this process.
//!
//! A CONNECTION function is code of the same runtime, stored by the same kind of create and
//! held to the same quota, so it runs HERE too ([`run_connection_function`]) rather than on a
//! second engine. Its programming model is the other one: the code runs over the connection
//! CloudFront is deciding about, which this module puts in scope as the global `connection`
//! and hands to a `handler` the code declares, and the DECISION it takes on that connection
//! (`connection.allow()` or `connection.deny()`) is what the run produced.
//!
//! "CloudFront runs the function, passing it the event object that you provided, and returns
//! the function's result (the modified event object) in the response. The response also
//! contains function logs and error messages, if any exist." Every one of those three is
//! whatever the caller's OWN code produced, so none of them can be assembled here: the code
//! is compiled and executed by the QuickJS engine embedded in this process (`rquickjs`), the
//! function's global `handler` is CALLED over the event object, and the value it returned,
//! the lines it logged and the failure it raised are read back off that run.
//!
//! # What the code can reach
//!
//! A FRESH engine per run, so nothing of one run is reachable from the next: no global and
//! no value outlives the call that made it. Inside that engine the code reaches the language
//! and ONE host function — the log writer the console is built over, which only appends to
//! this run's own captured lines and is deleted from the global object before any of the
//! caller's code is compiled, so only the console closure still holds it. A connection
//! function's code also reaches the connection in scope, whose decisions answer the run
//! through a channel the code cannot reach. No module loader is installed, so an `import`
//! resolves to nothing; plain QuickJS registers no filesystem, process, environment,
//! network, host-library or thread API, and nothing here adds one.
//!
//! # What bounds it
//!
//! Every bound lives in host code the running function cannot reach:
//!
//! * a WALL CLOCK ([`RUN_WALL_CLOCK`]) and a STEP budget ([`RUN_ENGINE_CHECKS`]), both spent
//!   by the engine's own interrupt handler as it works, so code that never returns and code
//!   that loops without allocating are each stopped;
//! * a MEMORY ceiling ([`RUN_MEMORY_BYTES`]) and a JavaScript STACK ceiling
//!   ([`RUN_STACK_BYTES`]), both the engine's own, so an allocation or a recursion past them
//!   ends the run inside the engine rather than on the host's stack;
//! * ceilings on what one run may REPORT: how many log lines ([`MAX_LOG_ENTRIES`]), how long
//!   one line ([`MAX_LOG_ENTRY_BYTES`]) and how large the returned event object
//!   ([`MAX_OUTPUT_BYTES`]) may be;
//! * a bound on the JOBS a returned promise may settle through ([`MAX_PENDING_JOBS`]);
//! * a CONCURRENCY permit ([`MAX_CONCURRENT_RUNS`]) and a CALLER WAIT ceiling
//!   ([`CALLER_WAIT_CEILING`]) held outside the engine, so however many callers run
//!   functions at once, what the engine holds is bounded and each request is answered even
//!   where a stretch of engine-internal work outlasts the run's own clock.
//!
//! Which bound was breached is LATCHED in host state ([`Bounds`]) rather than read back out
//! of the engine, so code that catches the exception the engine raises in it cannot make the
//! run report a result: once the latch is set, the run's answer is the stop.
//!
//! The run happens on a BLOCKING thread ([`run_function`]), never on the async executor
//! serving the request, and no store lock is held across it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::rquickjs::function::Func;
use ::rquickjs::{CatchResultExt, Context, Ctx, Function, Runtime, Value};
use ::serde_json::Value as Json;

/// The global a CloudFront function's code declares and CloudFront calls: "the function code
/// ... declares a function named `handler`", which is handed the event object and whose
/// return value is the result.
pub const HANDLER_NAME: &str = "handler";

/// The member that marks the object a function returned as a RESPONSE rather than a request:
/// a response object "must contain `statusCode`", while a request object has no such member.
/// It is what decides which member of the returned event object the result is reported under.
pub const RESPONSE_STATUS_MEMBER: &str = "statusCode";

/// The member the returned event object is reported under when the function returned a
/// REQUEST — the shape `FunctionOutput` carries ("the event object returned by the
/// function"), which holds the request or the response the function came back with.
pub const OUTPUT_REQUEST_MEMBER: &str = "request";

/// The member the returned event object is reported under when the function returned a
/// RESPONSE, the other half of the pair [`OUTPUT_REQUEST_MEMBER`] documents.
pub const OUTPUT_RESPONSE_MEMBER: &str = "response";

/// The maximum time CloudFront allows one function to run in, which `ComputeUtilization` is
/// a percentage OF: "The amount of time that the function took to run as a percentage of the
/// maximum allowed time." CloudFront's published quota for the execution time of a
/// CloudFront function is 1 millisecond, so that is the denominator; the numerator is the
/// time this engine measured for the run ([`FunctionRun::compute_utilization`]).
///
/// It is NOT what stops a run — this emulator's engine is slower than CloudFront's edge, so
/// stopping at a millisecond would refuse code CloudFront runs. [`RUN_WALL_CLOCK`] is the
/// bound a run is actually held to.
pub const MAX_FUNCTION_EXECUTION_TIME: Duration = Duration::from_millis(1);

/// How long one run may take before the engine's interrupt handler stops it.
///
/// A bound of this EMULATOR's own, not a documented CloudFront one: the code runs in this
/// process, so a function with no exit would otherwise hold a thread for ever.
const RUN_WALL_CLOCK: Duration = Duration::from_secs(2);

/// How long the CALLER waits for a run at most, which is longer than the run's own clock by
/// the margin it takes the engine to notice that clock.
///
/// A ceiling of the HOST's, held outside the engine: the engine can only be stopped at a
/// check it makes, so a stretch of engine-internal work can outlast the run's own clock.
/// This is what answers the request anyway.
const CALLER_WAIT_CEILING: Duration = Duration::from_secs(5);

/// How many engine checks one run may spend — the STEP bound beside the wall-clock one, and
/// the one that does not depend on how fast the host is. The engine asks its interrupt
/// handler once per fixed number of bytecode operations, so a budget of asks is a budget of
/// work: code that diverges is stopped after the same amount of WORK on every machine.
const RUN_ENGINE_CHECKS: u64 = 100_000;

/// The heap one run's engine is built with.
const RUN_MEMORY_BYTES: usize = 32 * 1024 * 1024;

/// The JavaScript stack one run's engine is built with. Well inside the stack of the
/// blocking thread the run happens on, so deep recursion in a function ends that run with
/// the engine's own stack-overflow exception rather than touching the thread's guard page.
const RUN_STACK_BYTES: usize = 512 * 1024;

/// How large the returned event object may be, in bytes, before the run is reported as
/// having failed: the modelled maximum length of `FunctionEventObject` (`@length 0..40960`),
/// which is the largest event object this operation carries in either direction.
pub const MAX_OUTPUT_BYTES: usize = 40_960;

/// How many log lines one run may report, which is what `FunctionExecutionLogs` carries
/// ("the log lines that the function wrote (if any) when running the test").
pub const MAX_LOG_ENTRIES: usize = 100;

/// How long one reported log line may be, in bytes.
pub const MAX_LOG_ENTRY_BYTES: usize = 4_096;

/// How many engine jobs one run may settle a returned promise through, so an async function
/// that keeps queueing work cannot make the drain below unbounded.
const MAX_PENDING_JOBS: usize = 1_000;

/// How many functions this emulator runs AT ONCE.
///
/// An EMULATOR bound: each run holds the engine's own heap on a blocking thread, so this is
/// what bounds what those runs can hold and occupy however many callers arrive together. The
/// permit is held by the RUN rather than by the caller waiting on it
/// ([`run_function`]), so a run the caller gave up on keeps its slot until the engine thread
/// really ends: at most this many engines exist at any instant, whatever a caller does.
const MAX_CONCURRENT_RUNS: usize = 4;

/// How long a caller waits for one of those run slots before the run is reported as one that
/// did not happen, rather than queueing behind other callers' code.
const RUN_PERMIT_WAIT: Duration = Duration::from_secs(5);

/// The run slots [`MAX_CONCURRENT_RUNS`] declares, over every function of this process: the
/// bound is on what the ENGINE may hold at once, which is a property of the process rather
/// than of one function or one request.
///
/// Held as an `Arc` so a permit can be OWNED by the blocking run it bounds rather than by the
/// future awaiting it.
fn function_runs() -> &'static ::std::sync::Arc<::tokio::sync::Semaphore> {
    static RUNS: ::std::sync::OnceLock<::std::sync::Arc<::tokio::sync::Semaphore>> =
        ::std::sync::OnceLock::new();
    RUNS.get_or_init(|| {
        ::std::sync::Arc::new(::tokio::sync::Semaphore::new(MAX_CONCURRENT_RUNS))
    })
}

/// What one run of a function came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionOutcome {
    /// The function returned an event object: the JSON text of it, under the member its
    /// shape names ([`OUTPUT_REQUEST_MEMBER`] or [`OUTPUT_RESPONSE_MEMBER`]).
    Returned(String),
    /// The function FAILED, and this is the error message: the code did not compile, it
    /// declared no handler, it raised, it returned something that is not an event object, or
    /// a bound of this module stopped it.
    Failed(String),
    /// The code RAN and came back without answering, and nothing about it failed: a
    /// connection function that took no decision about the connection it was run over,
    /// declared no handler to take one, and whose connection could not be read back out of the
    /// engine. There is nothing to report but the lines it logged and the time it spent — and
    /// no error either, because nothing went wrong. A handler model run has no such outcome:
    /// a handler that never settles is a run that failed.
    Unanswered,
    /// The run did not happen at all — the engine did not start, every run slot was taken,
    /// or the host gave up waiting for it. Nothing was learned about the code, which is a
    /// different answer from "the function failed".
    Unavailable(String),
}

impl FunctionOutcome {
    /// What a TEST RESULT reports about this outcome, or the reason the run did not happen at
    /// all — which is no result, and is answered as the test having failed instead.
    ///
    /// A test result that is served at all reports BOTH halves of what a run produced: the
    /// output document and the error message. A run that returned an event object reports it
    /// with NO error message — the empty string, which is what "there was no error" is said
    /// with — and a run that failed reports its error message beside an EMPTY event object
    /// ([`EMPTY_EVENT_OBJECT`]): the function's result IS the object it returned, so a run
    /// that returned none produced no object, and nothing it did to the one it was handed
    /// leaks past the failure. ONE mapping for every function test of this service, so the
    /// two kinds of function report a run the same way.
    pub fn reported(self) -> Result<ReportedRun, String> {
        match self {
            // "Returns the function's result (the modified event object) in the response."
            Self::Returned(output) => Ok(ReportedRun::returned(output)),
            // "If the result of testing the function was an error, this field contains the
            // error message": the code ran and failed, which is a result the test reports
            // with the error message of that failure rather than a server fault.
            Self::Failed(message) => Ok(ReportedRun::failed(message)),
            // The code ran, came back without answering and failed at nothing: there is no
            // object it produced to report, and no error either.
            Self::Unanswered => Ok(ReportedRun::returned(EMPTY_EVENT_OBJECT.to_string())),
            // The run did not happen at all, so there is no result to report and nothing is
            // known about the code.
            Self::Unavailable(reason) => Err(reason),
        }
    }
}

/// What a function test reports about one run that HAPPENED.
///
/// Both members are always reported: see [`FunctionOutcome::reported`], the one place they
/// are decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedRun {
    /// The event object the run produced, as the JSON document the result carries
    /// (`TestResult$FunctionOutput`, "the event object returned by the function").
    pub output: String,
    /// The failure the run reported, or the EMPTY STRING for a run that reported none
    /// (`TestResult$FunctionErrorMessage`, "if the result of testing the function was an
    /// error, this field contains the error message").
    pub error_message: String,
}

impl ReportedRun {
    /// A run that produced `output` and failed at nothing.
    fn returned(output: String) -> Self {
        Self {
            output,
            error_message: String::new(),
        }
    }

    /// A run that failed with `message`, and so produced no event object of its own.
    ///
    /// The reported message is the service's FIXED sentence for a function that could not be
    /// run as it stands ([`FUNCTION_FAILED_SENTENCE`]) followed by `message`, the account the
    /// runtime gave of the failure: the sentence says WHAT failed, which is the same for every
    /// failure of every function, and the variable part says how.
    fn failed(message: String) -> Self {
        Self {
            output: EMPTY_EVENT_OBJECT.to_string(),
            error_message: format!("{FUNCTION_FAILED_SENTENCE}{message}"),
        }
    }
}

/// The fixed part of the error message a function test reports for a run that FAILED: the
/// sentence identifying the function as one that is invalid or could not run, which every
/// such message opens with, and which the message the runtime gave is appended to.
///
/// It names no function, no run and no value, because it is the whole fixed text: everything
/// about the particular failure is in the variable part that follows it.
pub const FUNCTION_FAILED_SENTENCE: &str =
    "The CloudFront function associated with the CloudFront distribution is invalid or could \
     not run. Error: ";

/// The event object a run that produced none is reported with: the empty JSON object
/// document.
///
/// A result reports an output document whether or not the run came back with one, so the
/// document for "the run produced none" is the object with no members in it — never a
/// fragment of the event object the request carried, which a failed run did not return.
pub const EMPTY_EVENT_OBJECT: &str = "{}";

/// Everything one run of a function produced.
#[derive(Debug, Clone)]
pub struct FunctionRun {
    /// What the run came to.
    pub outcome: FunctionOutcome,
    /// The lines the function wrote through the console, in the order it wrote them.
    pub logs: Vec<String>,
    /// How long the engine spent compiling and running the code, which is what the reported
    /// compute utilization is measured from.
    pub elapsed: Duration,
}

impl FunctionRun {
    /// `TestResult$ComputeUtilization` for this run: "the amount of time that the function
    /// took to run as a percentage of the maximum allowed time"
    /// ([`MAX_FUNCTION_EXECUTION_TIME`]), as the whole-number percentage the member carries.
    ///
    /// Rounded UP, so a run that took measurable time never reports none of the budget, and
    /// held to 100: a run that outlasted the allowed time used all of it.
    pub fn compute_utilization(&self) -> String {
        let allowed = MAX_FUNCTION_EXECUTION_TIME.as_micros().max(1);
        let spent = self.elapsed.as_micros();
        let percentage = spent.saturating_mul(100).div_ceil(allowed).min(100);
        percentage.to_string()
    }

    /// A run that did not happen, carrying `reason`.
    fn unavailable(reason: String) -> Self {
        Self {
            outcome: FunctionOutcome::Unavailable(reason),
            logs: Vec::new(),
            elapsed: Duration::ZERO,
        }
    }
}

/// The programming model one run follows: how the code is INVOKED, and how what it produced
/// is reported.
///
/// Everything else about a run — the engine, every bound it is held to, the console it logs
/// through and the clock the compute utilization is measured against — is the same for both
/// models, so the two kinds of function share this module rather than each carrying an
/// engine of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Model {
    /// A CloudFront function: the code "declares a function named `handler`", which is
    /// CALLED with the event object, and the result is "the function's result (the modified
    /// event object)" that call came back with ([`event_output`]).
    Handler,
    /// A connection function: the code runs over the connection it decides about, in scope as
    /// the global `connection` and handed to a `handler` it may declare, and the result is the
    /// DECISION it took on that connection ([`returned_output`]). Code that only decides is a
    /// whole connection function — and so is code that decides nothing, which is why taking no
    /// decision is not a failure.
    ConnectionScript,
}

impl Model {
    /// What the result reports for a run whose answer was `text` (the JSON the engine
    /// rendered it as), or the message of an answer that cannot be reported.
    fn output(self, text: &str) -> Result<String, String> {
        match self {
            Self::Handler => event_output(text),
            Self::ConnectionScript => returned_output(text),
        }
    }

    /// What a run that came back without answering at all comes to.
    ///
    /// A HANDLER that returned a promise nothing settled produced no event object, which is
    /// the run failing — the model states the function's result IS the event object it
    /// returns. A connection function that took no decision about the connection failed at
    /// nothing: the model requires no decision of a connection function, so what the run
    /// produced is the connection as the code left it, read back out of the engine, and this
    /// is the answer only for a run whose connection could not be read back at all — its logs
    /// and the time it spent, and neither an output nor an error.
    fn unanswered(self) -> FunctionOutcome {
        match self {
            Self::Handler => FunctionOutcome::Failed(UNSETTLED.to_string()),
            Self::ConnectionScript => FunctionOutcome::Unanswered,
        }
    }
}

/// Run the CloudFront function `code` over the event object `event` (the JSON text of it),
/// reporting what its handler returned as the event object a `TestResult` carries.
///
/// See [`run_on_engine`] for what bounds the run.
pub async fn run_function(code: Vec<u8>, event: String) -> FunctionRun {
    run_on_engine(code, event, Model::Handler).await
}

/// Run the connection function `code` over the connection object `connection_object` (the
/// JSON text of it), reporting the decision it took on that connection as the output a
/// `ConnectionFunctionTestResult` carries.
///
/// A connection function is code of the same runtime, stored by the same create and held to
/// the same quota as a CloudFront function, so it runs on the same engine under the same
/// bounds ([`run_on_engine`]); what differs is the programming model: the code decides about
/// the connection in scope, rather than returning an event object.
pub async fn run_connection_function(code: Vec<u8>, connection_object: String) -> FunctionRun {
    run_on_engine(code, connection_object, Model::ConnectionScript).await
}

/// Run `code` over the document `argument` (the JSON text of the object the run is handed),
/// on a blocking thread, under the programming model `model`.
///
/// `code` is the code of a STORED function, so its size is already held to the published
/// function-code quota by the create and the update that wrote it
/// ([`super::MAX_FUNCTION_CODE_BYTES`]); this module bounds what RUNNING it may spend.
///
/// The engine, the bounds and the whole run live inside that thread, so nothing of the
/// engine crosses onto the executor serving the request, and the caller's wait is bounded
/// separately ([`CALLER_WAIT_CEILING`]): a run the engine has not come back from by then is
/// given up on, so the request is answered whatever the engine is doing — the given-up run
/// has already spent its own clock, so it ends at the next check the engine makes.
///
/// The run slot is OWNED by that thread rather than by this future: dropping the join handle
/// does not stop a blocking task, so releasing the permit when the caller stops waiting
/// would let a given-up run keep its engine while its slot was handed to the next caller,
/// and repeated calls could then hold any number of engines at once. Held by the run, the
/// slot comes back only when the engine thread ends, so [`MAX_CONCURRENT_RUNS`] bounds the
/// engines that exist whatever a caller does; a caller that finds every slot still taken is
/// answered as a run that did not happen.
async fn run_on_engine(code: Vec<u8>, argument: String, model: Model) -> FunctionRun {
    let waited = ::tokio::time::timeout(
        RUN_PERMIT_WAIT,
        ::std::sync::Arc::clone(function_runs()).acquire_owned(),
    )
    .await;
    let permit = match waited {
        Ok(Ok(permit)) => permit,
        _ => {
            return FunctionRun::unavailable(format!(
                "every one of the {MAX_CONCURRENT_RUNS} run slots this emulator holds was \
                 taken for {} seconds",
                RUN_PERMIT_WAIT.as_secs()
            ))
        }
    };
    let running = ::tokio::task::spawn_blocking(move || {
        // The slot is released where the RUN ends, on every path out of it.
        let _slot = permit;
        run(code, &argument, model)
    });
    match ::tokio::time::timeout(CALLER_WAIT_CEILING, running).await {
        Ok(Ok(run)) => run,
        Ok(Err(error)) => FunctionRun::unavailable(format!(
            "the thread the function ran on did not finish: {error}"
        )),
        Err(_) => FunctionRun::unavailable(format!(
            "the function was given up on after {} seconds",
            CALLER_WAIT_CEILING.as_secs()
        )),
    }
}

/// Run `code` over the document `argument` on THIS thread, under every bound this module
/// declares and under the programming model `model`.
fn run(code: Vec<u8>, argument: &str, model: Model) -> FunctionRun {
    let collected = Arc::new(Mutex::new(Collected::default()));
    // The code is a blob on the wire, so it may be bytes no engine can read at all. That is
    // a property of the code rather than of this emulator, so it is the function failing.
    let Ok(source) = String::from_utf8(code) else {
        return finished(
            FunctionOutcome::Failed(NOT_TEXT.to_string()),
            &collected,
            Duration::ZERO,
        );
    };
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            return FunctionRun::unavailable(format!("the engine did not start: {error}"))
        }
    };
    runtime.set_memory_limit(RUN_MEMORY_BYTES);
    runtime.set_max_stack_size(RUN_STACK_BYTES);
    let bounds = Arc::new(Bounds::new());
    let watched = Arc::clone(&bounds);
    // The only thing that can stop code with no exit: a timeout cannot preempt a loop
    // running on this thread, but the engine asks this handler as it works.
    runtime.set_interrupt_handler(Some(Box::new(move || watched.spent())));
    let context = match Context::full(&runtime) {
        Ok(context) => context,
        Err(error) => {
            return FunctionRun::unavailable(format!("the engine did not start: {error}"))
        }
    };

    let answer: Arc<Mutex<Option<Answered>>> = Arc::new(Mutex::new(None));
    let (invoked, mut elapsed) =
        context.with(|ctx| invoke(&ctx, &source, argument, &collected, &answer, model));
    // Code that has not answered yet may have queued the work that answers for it — a handler
    // that returned a promise, a script that decided inside one — and nothing else of this run
    // drives those jobs.
    if matches!(invoked, Invoked::Pending) {
        let started = Instant::now();
        drain_jobs(&runtime, &bounds);
        elapsed += started.elapsed();
    }
    // A breached bound is the run's answer, whatever the code then said or returned: the
    // latch is host state the code cannot reach. Read BEFORE anything re-enters the engine,
    // so a run the clock or the step budget already stopped is answered without running a
    // line more of the caller's code.
    let breach = bounds.breach();

    // The CONNECTION as the code LEFT it, read back out of the engine while this run's bounds
    // are STILL IN FORCE: what a connection function's run produced is the connection it was
    // deciding about with whatever the code did to it, so a run that took no decision of its
    // own still reports that connection rather than nothing ([`Model::unanswered`]). Read from
    // the engine rather than assembled here, so it is the document the code left behind.
    //
    // Rendering it runs the caller's own code again — a `toJSON` or an accessor the code put on
    // the connection is called by the renderer — so it happens with the interrupt handler still
    // installed and the same budget still being spent: code that diverges in a `toJSON` is
    // stopped by the clock and the step count exactly as code that diverges in the body is, and
    // a run already stopped is not re-entered at all.
    let left = match (model, breach.as_ref()) {
        (Model::ConnectionScript, None) => context.with(|ctx| {
            ctx.globals()
                .get::<_, Value<'_>>(CONNECTION_GLOBAL)
                .ok()
                .and_then(|connection| ctx.json_stringify(connection).ok().flatten())
                .and_then(|text| text.to_string().ok())
        }),
        _ => None,
    };

    // The run is over, and nothing the engine does to tear itself down is the function's
    // work: the interrupt handler is taken off first, then any exception a bound left
    // pending in the context is taken, so nothing of a stopped run is held at teardown.
    runtime.set_interrupt_handler(None);
    context.with(|ctx| drop(ctx.catch()));

    // The stop a bound latched, including one the read-back above spent the last of the budget
    // on: either way what the run came to is the stop.
    if let Some(breach) = breach.or_else(|| bounds.breach()) {
        return finished(FunctionOutcome::Failed(breach), &collected, elapsed);
    }
    let outcome = match invoked {
        Invoked::Unavailable(reason) => FunctionOutcome::Unavailable(reason),
        Invoked::Failed(message) => FunctionOutcome::Failed(message),
        Invoked::Settled | Invoked::Pending => {
            match answer.lock().unwrap_or_else(PoisonError::into_inner).clone() {
                Some(Answered::Returned(text)) => match model.output(&text) {
                    Ok(output) => FunctionOutcome::Returned(output),
                    Err(message) => FunctionOutcome::Failed(message),
                },
                Some(Answered::Raised(message)) => FunctionOutcome::Failed(message),
                // A connection function that took no decision of its own produced the
                // connection it was handed, as it left it: its own mutations are what that run
                // came to. A run whose connection could not be read back has nothing to report
                // but what it did ([`Model::unanswered`]).
                None => match left {
                    Some(document) => match model.output(&document) {
                        Ok(output) => FunctionOutcome::Returned(output),
                        Err(message) => FunctionOutcome::Failed(message),
                    },
                    None => model.unanswered(),
                },
            }
        }
    };
    finished(outcome, &collected, elapsed)
}

/// The whole run: `outcome`, plus the lines it wrote and the time it spent.
fn finished(
    outcome: FunctionOutcome,
    collected: &Arc<Mutex<Collected>>,
    elapsed: Duration,
) -> FunctionRun {
    let collected = collected.lock().unwrap_or_else(PoisonError::into_inner);
    FunctionRun {
        outcome,
        logs: collected.logs.clone(),
        elapsed,
    }
}

/// What the invocation itself came to, before the latch is read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Invoked {
    /// The function answered: the latch holds what it came to.
    Settled,
    /// The function returned a promise, so the latch is written by the job that settles it.
    Pending,
    /// The function failed before it could answer.
    Failed(String),
    /// Nothing of the function ran: the engine could not be prepared for it.
    Unavailable(String),
}

/// What the invoker reported the function came to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answered {
    /// The JSON text of the value the function returned.
    Returned(String),
    /// The message of what it raised, or of what made its answer unreadable.
    Raised(String),
}

/// Prepare the engine for this run and invoke the code as `model` says, inside one engine
/// context — and report what the invocation came to and how long the function's own work
/// took.
fn invoke(
    ctx: &Ctx<'_>,
    source: &str,
    argument: &str,
    collected: &Arc<Mutex<Collected>>,
    answer: &Arc<Mutex<Option<Answered>>>,
    model: Model,
) -> (Invoked, Duration) {
    if let Err(reason) = install_console(ctx, collected) {
        return (Invoked::Unavailable(reason), Duration::ZERO);
    }
    match model {
        Model::Handler => invoke_handler(ctx, source, argument, answer),
        Model::ConnectionScript => invoke_script(ctx, source, argument, answer),
    }
}

/// The channel one run's answer is reported through, as a host function the engine can call.
///
/// Passed as an ARGUMENT to this module's own code rather than installed as a global, so the
/// caller's code cannot reach it; the FIRST answer through it is the run's, so a later one
/// changes nothing.
fn settle_channel(answer: &Arc<Mutex<Option<Answered>>>) -> impl Fn(bool, String) + 'static {
    let latched = Arc::clone(answer);
    move |raised: bool, text: String| {
        let mut held = latched.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_none() {
            *held = Some(if raised {
                Answered::Raised(text)
            } else {
                Answered::Returned(text)
            });
        }
    }
}

/// The document `text` read by the engine's own JSON parser.
///
/// Its caller admits only a document that already parsed as a JSON object, and the text
/// handed here is that document re-rendered, so a failure here is the ENGINE not reading what
/// the host read — never a caller's malformed document, which its caller refuses as the
/// invalid argument it is.
fn parsed<'js>(ctx: &Ctx<'js>, text: &str, what: &str) -> Result<Value<'js>, Invoked> {
    ctx.json_parse(text.to_string())
        .map_err(|error| Invoked::Unavailable(format!("the {what} could not be read: {error}")))
}

/// Compile the code, call its handler over the event object and report what the call came
/// to — the CloudFront function model.
fn invoke_handler(
    ctx: &Ctx<'_>,
    source: &str,
    event: &str,
    answer: &Arc<Mutex<Option<Answered>>>,
) -> (Invoked, Duration) {
    // Held BEFORE any of the caller's code is compiled, so the invoker this run reads its
    // answer through is this module's own however the code rebinds the global object.
    let invoker = match ctx.eval::<Function<'_>, _>(INVOKER_SOURCE).catch(ctx) {
        Ok(invoker) => invoker,
        Err(caught) => {
            return (
                Invoked::Unavailable(format!("the engine did not start: {caught}")),
                Duration::ZERO,
            )
        }
    };
    let argument = match parsed(ctx, event, "event object") {
        Ok(argument) => argument,
        Err(unavailable) => return (unavailable, Duration::ZERO),
    };

    // From here on the clock is the FUNCTION's: its compile, and the call of its handler.
    let started = Instant::now();
    if let Err(caught) = ctx.eval::<Value<'_>, _>(source).catch(ctx) {
        return (Invoked::Failed(raised_message(&caught)), started.elapsed());
    }
    let Ok(handler) = ctx.globals().get::<_, Function<'_>>(HANDLER_NAME) else {
        return (Invoked::Failed(NO_HANDLER.to_string()), started.elapsed());
    };
    let settle = Func::from(settle_channel(answer));
    if let Err(caught) = invoker.call::<_, ()>((handler, argument, settle)).catch(ctx) {
        // The invoker reports the code's own raise itself, so reaching here means the run
        // was interrupted out of it — which the latch has already recorded.
        return (Invoked::Failed(raised_message(&caught)), started.elapsed());
    }
    let pending = answer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_none();
    let invoked = if pending {
        Invoked::Pending
    } else {
        Invoked::Settled
    };
    (invoked, started.elapsed())
}

/// Put the connection in scope, run the code over it and report what it decided — the
/// connection function model.
///
/// A connection function is code that runs over the connection CloudFront is deciding about:
/// this module puts that connection in scope before any of the caller's code is compiled —
/// the connection object the request carried, carrying the two decisions the service lets code
/// take on it, `allow()` and `deny()` — and TAKING one of them is what answers the run, so the
/// result reports the connection as the code left it, carrying the decision that was taken
/// ([`CONNECTION_SOURCE`]).
///
/// The code reaches that connection the way the runtime hands it over: as the global
/// `connection` its body reads, and as the argument of the function its code DECLARES — under
/// the name the runtime names, or as the single function that code declared
/// ([`DECLARED_SOURCE`]), since the function a connection function declares is the one the
/// runtime calls. So the code runs first, and the function it declared is then called with the
/// connection object, through the one invoker this module calls a handler through
/// ([`INVOKER_SOURCE`]): a raise inside that function is reported exactly as a raise in the
/// code's own body is, and a function that comes back with a value reports that value. The
/// channel the answer travels through keeps the FIRST answer, so the decision the code took
/// stands over whatever it then returned.
///
/// Code that raises reports the raise; code that decided nothing reports the connection as it
/// left it, read back by the caller of this function.
fn invoke_script(
    ctx: &Ctx<'_>,
    source: &str,
    connection_object: &str,
    answer: &Arc<Mutex<Option<Answered>>>,
) -> (Invoked, Duration) {
    // Held BEFORE any of the caller's code is compiled, for the reason the handler model's
    // invoker is: what a decision does is this module's own however the code rebinds the
    // global object, and the channel it answers through is unreachable from that code.
    let prepare = match ctx.eval::<Function<'_>, _>(CONNECTION_SOURCE).catch(ctx) {
        Ok(prepare) => prepare,
        Err(caught) => {
            return (
                Invoked::Unavailable(format!("the engine did not start: {caught}")),
                Duration::ZERO,
            )
        }
    };
    let invoker = match ctx.eval::<Function<'_>, _>(INVOKER_SOURCE).catch(ctx) {
        Ok(invoker) => invoker,
        Err(caught) => {
            return (
                Invoked::Unavailable(format!("the engine did not start: {caught}")),
                Duration::ZERO,
            )
        }
    };
    let connection = match parsed(ctx, connection_object, "connection object") {
        Ok(connection) => connection,
        Err(unavailable) => return (unavailable, Duration::ZERO),
    };
    let settle = Func::from(settle_channel(answer));
    if let Err(caught) = prepare
        .call::<_, ()>((connection.clone(), settle))
        .catch(ctx)
    {
        return (
            Invoked::Unavailable(format!("the connection could not be put in scope: {caught}")),
            Duration::ZERO,
        );
    }

    // What the code DECLARED, read once the code has run: prepared before any of it is
    // compiled, so the globals it compares against are this module's own and not the code's.
    let finder = match ctx.eval::<Function<'_>, _>(DECLARED_SOURCE).catch(ctx) {
        Ok(finder) => finder,
        Err(caught) => {
            return (
                Invoked::Unavailable(format!("the engine did not start: {caught}")),
                Duration::ZERO,
            )
        }
    };

    // From here on the clock is the FUNCTION's: compiling and running its code IS the
    // invocation.
    let started = Instant::now();
    let declared = match ctx.eval::<Value<'_>, _>(source).catch(ctx) {
        // The code's own value, where it IS a function: code whose body is the function it
        // declares has declared it that way.
        Ok(value) if value.is_function() => value,
        // Otherwise the function the code DECLARED, which is what the runtime calls
        // ([`DECLARED_SOURCE`]).
        Ok(_) => match finder.call::<_, Value<'_>>(()).catch(ctx) {
            Ok(declared) => declared,
            Err(caught) => return (Invoked::Failed(raised_message(&caught)), started.elapsed()),
        },
        Err(caught) => return (Invoked::Failed(raised_message(&caught)), started.elapsed()),
    };
    // The handler form: code that has not decided in its own body may have declared the
    // handler that decides, which is called with the connection object it decides about.
    if answer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_none()
    {
        if let Some(handler) = declared.into_function() {
            let settle = Func::from(settle_channel(answer));
            if let Err(caught) = invoker
                .call::<_, ()>((handler, connection, settle))
                .catch(ctx)
            {
                // The invoker reports the code's own raise itself, so reaching here means the
                // run was interrupted out of it — which the latch has already recorded.
                return (Invoked::Failed(raised_message(&caught)), started.elapsed());
            }
        }
    }
    let pending = answer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_none();
    // Code that has not decided yet may have queued the work that decides for it, so the run
    // is reported as pending and the jobs it queued are drained before its answer is read.
    let invoked = if pending {
        Invoked::Pending
    } else {
        Invoked::Settled
    };
    (invoked, started.elapsed())
}

/// The code that reads back WHICH function a connection function's code declared.
///
/// It is prepared before any of the caller's code is compiled: it records the globals that
/// exist then, and the function it answers afterwards is one the CODE declared — the one the
/// runtime names (`handler`) where the code declares that name, and otherwise the single
/// function the code declared, which is the function it has to run. Code that declared no
/// function, or more than one under names the runtime does not name, has declared no handler
/// and answers `null`: there is nothing a caller said to call.
///
/// An expression rather than a global, for the reason [`INVOKER_SOURCE`] is one.
const DECLARED_SOURCE: &str = r#"
(function () {
    const before = Object.create(null);
    for (const key of Object.getOwnPropertyNames(globalThis)) {
        before[key] = true;
    }
    return function () {
        if (typeof globalThis.handler === "function") {
            return globalThis.handler;
        }
        let found = null;
        let declared = 0;
        for (const key of Object.getOwnPropertyNames(globalThis)) {
            if (before[key] === true) {
                continue;
            }
            if (typeof globalThis[key] === "function") {
                found = globalThis[key];
                declared = declared + 1;
            }
        }
        return declared === 1 ? found : null;
    };
})()
"#;

/// Run the engine's pending jobs until the returned promise settles, the queue empties, the
/// job bound is spent or a bound of this run ends it.
fn drain_jobs(runtime: &Runtime, bounds: &Arc<Bounds>) {
    for _ in 0..MAX_PENDING_JOBS {
        if bounds.stop_if_due() {
            return;
        }
        match runtime.execute_pending_job() {
            // A job that ended on an exception nothing in the function handled leaves the
            // promise unsettled, which the latch then reports.
            Ok(true) => {}
            Ok(false) | Err(_) => return,
        }
    }
}

/// The result `TestResult$FunctionOutput` carries for a function that returned `text`: the
/// event object it came back with, under the member its own shape names.
///
/// "The event object returned by the function" is a request or a response, and a response
/// object is the one carrying [`RESPONSE_STATUS_MEMBER`] — so which member the returned
/// object travels under is decided by the object itself. The text the engine rendered is
/// reported as it stands rather than re-serialised here, so what a caller reads is what the
/// function returned.
fn event_output(text: &str) -> Result<String, String> {
    let Ok(returned) = ::serde_json::from_str::<Json>(text) else {
        return Err(RETURNED_UNREADABLE.to_string());
    };
    let Json::Object(members) = returned else {
        return Err(RETURNED_NO_EVENT_OBJECT.to_string());
    };
    let member = if members.contains_key(RESPONSE_STATUS_MEMBER) {
        OUTPUT_RESPONSE_MEMBER
    } else {
        OUTPUT_REQUEST_MEMBER
    };
    let output = format!("{{\"{member}\":{text}}}");
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "The function returned an event object of {} bytes, and at most \
             {MAX_OUTPUT_BYTES} bytes are allowed",
            output.len()
        ));
    }
    Ok(output)
}

/// The result `ConnectionFunctionTestResult$ConnectionFunctionOutput` carries for a
/// connection function whose run answered with `text` (the JSON the engine rendered that
/// answer as: the connection the code decided about, carrying the decision it took).
///
/// Reported as it stands rather than re-serialised or wrapped in a member of this emulator's
/// choosing — "the connection function output" is a plain string the model states no
/// structure for — so what a caller reads is what the code's own decision produced. The one
/// thing checked is that it is within what this operation's documents carry
/// ([`MAX_OUTPUT_BYTES`]), and a value past that is the run failing, as an event object past
/// it is.
fn returned_output(text: &str) -> Result<String, String> {
    if text.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "The function answered with a connection of {} bytes, and at most \
             {MAX_OUTPUT_BYTES} bytes are allowed",
            text.len()
        ));
    }
    Ok(text.to_string())
}

/// The message a raise carries: the account the ENGINE gives of what was raised.
///
/// A JavaScript error stringifies as its NAME and its message joined by `": "`
/// (`Error.prototype.toString`), and the name is what tells a caller which KIND of failure
/// the run had — a `SyntaxError` from code that does not parse is a different answer from an
/// error the code threw itself — so the name is reported beside the message WHERE IT NAMES
/// that kind. The name [`GENERIC_ERROR_NAME`] names none: it is the label the sentence every
/// reported failure opens with already ends in ([`FUNCTION_FAILED_SENTENCE`]), so an error
/// carrying it contributes only its message and the label is not said twice. An error
/// carrying only one of the two halves is reported as that one, and a raised value that is no
/// error object at all as the engine renders that value.
fn raised_message(caught: &::rquickjs::CaughtError<'_>) -> String {
    match caught {
        ::rquickjs::CaughtError::Exception(exception) => {
            let message = exception.message().unwrap_or_default();
            let name = exception
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .unwrap_or_default();
            match (name.trim(), message.trim()) {
                ("", "") => exception.to_string(),
                ("", message) => message.to_string(),
                (name, "") => name.to_string(),
                (GENERIC_ERROR_NAME, message) => message.to_string(),
                (name, message) => format!("{name}: {message}"),
            }
        }
        other => other.to_string(),
    }
}

/// The name a JavaScript error carries when it names no KIND of failure — the base `Error`
/// constructor's own name, which is also the label [`FUNCTION_FAILED_SENTENCE`] ends in. A
/// failure reported under this name is reported by its message alone, so the label the
/// sentence already states is not repeated inside the variable part.
const GENERIC_ERROR_NAME: &str = "Error";

/// The error message reported for code that is not text at all, so no engine can read it.
const NOT_TEXT: &str = "The function code is not valid UTF-8 text, so it cannot be compiled.";

/// The error message reported for code that compiles but declares no handler to call.
const NO_HANDLER: &str = "The function code does not declare a handler function.";

/// The error message reported for a function whose returned promise never settled, so the
/// run produced no event object.
const UNSETTLED: &str =
    "The function returned a promise that did not settle, so it produced no event object.";

/// The error message reported for a returned value the engine could not render as JSON.
const RETURNED_UNREADABLE: &str = "The function returned a value that is not valid JSON.";

/// The error message reported for a returned value that is not an event object at all.
const RETURNED_NO_EVENT_OBJECT: &str =
    "The function did not return an event object: a function must return the request or the \
     response object.";

/// The log lines one run wrote, under this module's ceilings.
#[derive(Debug, Default)]
struct Collected {
    logs: Vec<String>,
}

impl Collected {
    /// Record one line, under the count and length ceilings, cut on a character boundary so
    /// a capped line is still a string.
    fn log(&mut self, message: &str) {
        if self.logs.len() >= MAX_LOG_ENTRIES {
            return;
        }
        let mut line = message.to_string();
        if line.len() > MAX_LOG_ENTRY_BYTES {
            line.truncate(boundary_at_or_below(&line, MAX_LOG_ENTRY_BYTES));
        }
        self.logs.push(line);
    }
}

/// The largest byte index at or below `limit` that `text` can be cut at.
fn boundary_at_or_below(text: &str, limit: usize) -> usize {
    text.char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= limit)
        .last()
        .unwrap_or_default()
}

/// Install the console the function logs through: ONE host function, which only appends to
/// this run's captured lines, and the console written over it.
///
/// That host function is the only one the running code can reach, and the console source
/// deletes its global binding once it holds it, so the raw writer is reachable from nothing
/// but the console's own closure.
fn install_console(ctx: &Ctx<'_>, collected: &Arc<Mutex<Collected>>) -> Result<(), String> {
    let logged = Arc::clone(collected);
    ctx.globals()
        .set(
            HOST_LOG_NAME,
            Func::from(move |message: String| {
                logged
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .log(&message);
            }),
        )
        .map_err(|error| format!("the console could not be installed: {error}"))?;
    ctx.eval::<Value<'_>, _>(CONSOLE_SOURCE)
        .catch(ctx)
        .map(|_| ())
        .map_err(|caught| format!("the console could not be installed: {caught}"))
}

/// The global the console's writer is handed to the engine as, and which
/// [`CONSOLE_SOURCE`] removes again.
const HOST_LOG_NAME: &str = "__cfHostLog";

/// The console a CloudFront function writes its log lines through: `console.log` and its
/// neighbours, each rendering its arguments the way the engine renders a value and handing
/// the line to the host writer.
const CONSOLE_SOURCE: &str = r#"
(function () {
    var hostLog = globalThis.__cfHostLog;
    delete globalThis.__cfHostLog;
    var rendered = function (value) {
        if (typeof value === "string") { return value; }
        if (value instanceof Error) { return String(value); }
        try {
            var text = JSON.stringify(value);
            return text === undefined ? String(value) : text;
        } catch (error) {
            return String(value);
        }
    };
    var write = function () {
        hostLog(Array.prototype.map.call(arguments, rendered).join(" "));
    };
    globalThis.console = { log: write, info: write, debug: write, warn: write, error: write };
})()
"#;

/// The invoker: the ONE thing that calls the function's handler and reports what it came to,
/// over the channel it is handed.
///
/// It is an expression rather than a global, so the caller's code can neither read it nor
/// replace it, and it captures `JSON.stringify` and `String` before any of that code runs, so
/// code that rebinds either cannot change how its own answer is rendered. A handler that
/// returned a promise reports through the same channel when that promise settles.
///
/// A raise is described by the account the error itself carries: its `message`, preceded by
/// its `name` only where that name names the KIND of failure, so the kind reaches the caller
/// beside its wording while the generic error label is not repeated inside the variable part
/// of a reported message ([`FUNCTION_FAILED_SENTENCE`], [`GENERIC_ERROR_NAME`]). A raised
/// value that is no error object is described as the engine renders it (`String(value)`), and
/// a value whose conversion itself fails by one fixed sentence.
const INVOKER_SOURCE: &str = r#"
(function () {
    const stringify = JSON.stringify;
    const toText = String;
    const genericName = toText(Error.prototype.name);
    const described = function (error) {
        try {
            const held = error === null || error === undefined;
            const name = held ? null : error.name;
            const message = held ? null : error.message;
            const named = name === undefined || name === null ? "" : toText(name);
            const said = message === undefined || message === null ? "" : toText(message);
            if (said.length > 0) {
                if (named.length > 0 && named !== genericName) {
                    return named + ": " + said;
                }
                return said;
            }
            if (named.length > 0) {
                return named;
            }
        } catch (ignored) { }
        try {
            const text = toText(error);
            if (text !== undefined && text !== null && text.length > 0) {
                return text;
            }
        } catch (ignored) { }
        return "The function raised a value that cannot be read.";
    };
    return function (handler, event, settle) {
        const returned = function (value) {
            let text;
            try {
                text = stringify(value);
            } catch (error) {
                settle(true, described(error));
                return;
            }
            settle(false, text === undefined ? "null" : text);
        };
        let value;
        try {
            value = handler(event);
        } catch (error) {
            settle(true, described(error));
            return;
        }
        if (value !== null && typeof value === "object" && typeof value.then === "function") {
            try {
                value.then(returned, function (error) { settle(true, described(error)); });
            } catch (error) {
                settle(true, described(error));
            }
            return;
        }
        returned(value);
    };
})()
"#;

/// The code that puts the connection in scope: it installs the two DECISIONS the service lets
/// a connection function take — `allow()`, which states that the connection may proceed, and
/// `deny()`, which states that it may not — on the connection object the request carried, and
/// binds that object as the global `connection` the code reads.
///
/// Taking a decision RECORDS it on the connection it was taken on and ANSWERS the run with the
/// connection object as it then stands, so what a caller reads is the connection the code
/// decided about carrying the decision it took: the two decisions are the whole point of
/// testing a connection function, so a run that allowed a connection and a run that denied the
/// same one report different documents, and a client can tell a deny-by-default function from
/// an allowing one.
///
/// The decisions are installed on the connection object the request carried, which is the one
/// the code decides about and the one the handler it declares is called with.
///
/// An expression rather than a global, for the reason [`INVOKER_SOURCE`] is one: the caller's
/// code can neither read it nor replace it, and it captures `JSON.stringify` and `String`
/// before any of that code runs, so code that rebinds either cannot change how the connection
/// it decided about is rendered. The decisions report through the channel they are handed,
/// which the caller's code cannot reach. They are defined as NON-ENUMERABLE members, so the
/// connection a function walks — and the one the answer renders — is the document the request
/// carried, with the decision the code took and nothing else of this emulator's own.
const CONNECTION_SOURCE: &str = r#"
(function () {
    const stringify = JSON.stringify;
    const toText = String;
    return function (connection, settle) {
        const answer = function () {
            let text;
            try {
                text = stringify(connection);
            } catch (error) {
                settle(true, toText(error));
                return;
            }
            settle(false, text === undefined ? "null" : text);
        };
        const decision = function (object, taken) {
            return function () {
                object.decision = taken;
                answer();
            };
        };
        const install = function (object) {
            Object.defineProperty(object, "allow", {
                enumerable: false,
                value: decision(object, "allow")
            });
            Object.defineProperty(object, "deny", {
                enumerable: false,
                value: decision(object, "deny")
            });
        };
        install(connection);
        globalThis.connection = connection;
    };
})()
"#;

/// The global a connection function's code reads the connection it decides about through, and
/// the one the host reads the connection back out of once the run is over.
const CONNECTION_GLOBAL: &str = "connection";

/// The run's budget, as the engine's interrupt handler spends it, and the latch that says
/// which bound ended the run.
#[derive(Debug)]
struct Bounds {
    /// When the run must be over.
    deadline: Instant,
    /// How many engine checks this budget started with, which a breach reports.
    budget: u64,
    /// How many engine checks are left.
    checks: AtomicU64,
    /// Set once the clock ran out.
    timed_out: AtomicBool,
    /// Set once the checks ran out.
    steps_spent: AtomicBool,
}

impl Bounds {
    /// A budget starting now, of the checks and the clock one run is held to.
    fn new() -> Self {
        Self::with_budget(RUN_ENGINE_CHECKS)
    }

    /// A budget of `budget` checks starting now — the one constructor, so a budget small
    /// enough to spend in a test is spent through exactly the path a run's is.
    fn with_budget(budget: u64) -> Self {
        Self {
            deadline: Instant::now() + RUN_WALL_CLOCK,
            budget,
            checks: AtomicU64::new(budget),
            timed_out: AtomicBool::new(false),
            steps_spent: AtomicBool::new(false),
        }
    }

    /// Whether the run is over, asked by the engine as it works: the latch first, then the
    /// clock, then the step budget. Each answer latches, so the breach survives code that
    /// catches the exception the engine raises — and a spent budget STAYS spent: the count is
    /// decremented without underflowing, so however many times the engine asks after the
    /// budget ran out, the answer is still "over".
    fn spent(&self) -> bool {
        if self.steps_spent.load(Ordering::Relaxed) {
            return true;
        }
        if self.stop_if_due() {
            return true;
        }
        // `try_update` answers the count as it stood BEFORE this check spent a step, so a
        // `before` of one is the check that spends the last of the budget.
        let before = self
            .checks
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                Some(left.saturating_sub(1))
            })
            .unwrap_or_default();
        if before <= 1 {
            self.steps_spent.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Whether the run's clock has run out, latching it when it has. Read by the host where
    /// the engine is not running code of its own, so a run is stopped between jobs as well
    /// as inside one.
    fn stop_if_due(&self) -> bool {
        if Instant::now() >= self.deadline {
            self.timed_out.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// What the breached bound is reported as, or `None` when none was breached.
    fn breach(&self) -> Option<String> {
        if self.timed_out.load(Ordering::Relaxed) {
            return Some(format!(
                "The function was stopped after {} seconds without returning.",
                RUN_WALL_CLOCK.as_secs()
            ));
        }
        if self.steps_spent.load(Ordering::Relaxed) {
            return Some(format!(
                "The function was stopped after {} engine steps without returning.",
                self.budget
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::Bounds;

    /// The step budget is what stops code the clock has not caught, so it has to STAY spent:
    /// the engine keeps asking after a bound has been breached (every later check, and every
    /// job of a drained queue), and an answer of "keep going" there would hand the run its
    /// budget again. Asserted over a budget small enough that the clock cannot be what ends
    /// it, so what the run reports is the STEP bound on every machine.
    #[test]
    fn a_spent_step_budget_stays_spent() {
        const BUDGET: u64 = 8;

        let bounds = Bounds::with_budget(BUDGET);
        for step in 1..BUDGET {
            assert!(
                !bounds.spent(),
                "step {step}: a budget of {BUDGET} must not be spent yet"
            );
        }
        for step in BUDGET..(BUDGET + 1_000) {
            assert!(
                bounds.spent(),
                "step {step}: expected a spent budget to leave the run over"
            );
        }
        assert_eq!(
            bounds.breach(),
            Some(format!(
                "The function was stopped after {BUDGET} engine steps without returning."
            )),
            "a spent step budget is what the run reports"
        );
    }
}
