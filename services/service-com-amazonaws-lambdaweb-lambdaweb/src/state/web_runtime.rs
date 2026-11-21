// SPDX-License-Identifier: CC0-1.0

//! RUNNING a web function: the JavaScript engine a revision's deployment package executes
//! in, inside this process, and the one HTTP request a published endpoint hands it.
//!
//! "Your code listens on a network port and handles requests directly … You can use
//! familiar Node.js web frameworks … with no Lambda-specific adapters or wrappers"
//! (`smithy_query(query="doc", shape="LambdaWeb")`), so what a client reads back from an
//! endpoint's published address is the response THE REVISION'S OWN CODE produced. None of
//! that can be assembled here, so none of it is: the package's modules
//! ([`super::code_package::DeploymentPackage`]) are compiled and executed by the QuickJS
//! engine embedded in this process (`rquickjs`), the server the code registered is handed
//! the request the client sent, and the status, headers and body it answered are read back
//! off that run. A package that registers no server, or whose code raises before it
//! answers, produces no response, and that is reported as what it is — nothing is
//! synthesised in the handler's name.
//!
//! # What the guest can reach
//!
//! ONE engine per request, built here and dropped with the run, so nothing of one request's
//! memory is reachable from the next. Inside it the guest reaches the language, the modules
//! this module implements for it ([`node_runtime`]: `http`, `events`, `path`, `url`,
//! `querystring`, `util`, `os`, a console, timers and a `process` carrying the revision's
//! own environment variables) and the two host functions those are built over — the
//! response sink and the log writer — both deleted from the global object before any of the
//! guest's own code is compiled, so only this module's closures still hold them.
//!
//! Module resolution is this module's own ([`RuntimeResolver`], [`RuntimeLoader`], answered
//! by the runtime's own registry): an `import` or `require` resolves to a file of THIS
//! package or to one of the core modules above, and to nothing else, so no host path is a
//! module. Plain QuickJS registers no filesystem, process, environment, socket,
//! host-library or thread API of its own, and this module adds none: there is no `fs`, no
//! `net`, no `child_process` and no `worker_threads` to require, and the `process` the guest
//! reads holds the revision's own variables rather than anything of the process this
//! emulator runs in.
//!
//! # What bounds it
//!
//! Every bound lives in host code the guest cannot reach:
//!
//! * a WALL CLOCK ([`run_clock`], the revision's own `timeoutSeconds` clamped to the range
//!   the model allows and to [`RUN_WALL_CLOCK_CEILING`]) and a STEP budget
//!   ([`RUN_ENGINE_CHECKS`]), both spent by the engine's own interrupt handler as it works,
//!   so a guest that never returns and one that loops without allocating are each stopped;
//! * a MEMORY ceiling ([`RUN_MEMORY_BYTES`]) and a JavaScript STACK ceiling
//!   ([`ENGINE_STACK_BYTES`]), both the engine's own, well under the stack of the thread the
//!   run is given;
//! * ceilings on what one request may produce: the response body ([`MAX_RESPONSE_BYTES`],
//!   past which the run is reported as having produced no response the client can be served,
//!   rather than a prefix of the one the code wrote) and the log lines
//!   ([`MAX_LOG_ENTRIES`], [`MAX_LOG_ENTRY_BYTES`]);
//! * bounds on the work the run is driven through: the engine jobs ([`MAX_PENDING_JOBS`])
//!   and the timer callbacks ([`MAX_TIMER_TICKS`]) one request may spend;
//! * a LIVE-RUN ceiling ([`MAX_LIVE_RUNS`]) and a CALLER WAIT ceiling
//!   ([`CALLER_WAIT_CEILING`]) held outside the engine, so however many requests arrive at
//!   once, what the engines hold is bounded and every caller is answered even where a
//!   stretch of engine-internal work outlasts the run's own clock. The stop a breached bound
//!   lands is the engine's own interruption of the code it is executing, which is the only
//!   place an interpreter can be interrupted; a run that does not end there is ABANDONED —
//!   the caller is answered without it, its serving slot goes back at once, and its THREAD
//!   holds one place in a separate live-thread count ([`MAX_LIVE_RUN_THREADS`],
//!   [`RunAdmission`]) until it ends, so what abandoned runs can hold is bounded, a request
//!   that finds every place live is answered rather than started, and every abandonment is
//!   recorded at warn level.
//!
//! Which bound was breached is LATCHED in host state rather than read back out of the
//! engine, so a guest that catches the exception the engine raises in it cannot make the
//! run report a response.
//!
//! # The guest's clock
//!
//! A guest's timers run on a VIRTUAL clock: `setTimeout` records when a callback is due in
//! the guest's own time, and the host runs the earliest due callback whenever nothing else
//! can make progress. Nothing sleeps — a handler that waits a second before answering costs
//! the client no wall-clock time — and the ORDER the guest sees is the order it asked for.
//!
//! The run happens on a thread of its own with an explicit stack, never on the async
//! executor serving the request, and no store guard is held across it.
//!
//! # What this layer carries
//!
//! A request's body reaches the code as TEXT and the body it answers with is the text it
//! wrote: the bytes of a request that is not UTF-8 are not carried through this seam, and
//! the guest is given no byte-buffer type to write one with. So this layer serves the text
//! HTTP a web function framework answers with — HTML, JSON, plain text — and a binary
//! request or response is not yet carried; nothing here pretends otherwise.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use ::rquickjs::function::Func;
use ::rquickjs::loader::{ImportAttributes, Loader, Resolver};
use ::rquickjs::{CatchResultExt, Context, Ctx, Function, Module, Object, Runtime};

use super::code_package::DeploymentPackage;

/// The JavaScript the guest's modules run under: the core modules, the timers, the server
/// it registers and the registry its own files are resolved in.
mod node_runtime;

#[cfg(test)]
mod tests;

/// The longest ANY run of a web function may take, whatever its revision configured.
///
/// `ServiceConfig$timeoutSeconds` is the service's own bound on a run ("the amount of time
/// (in seconds) that Lambda allows the web function to run before stopping it", range
/// 3..900), and it is a value a CALLER chooses: it therefore decides when the GUEST is
/// stopped and never how long this process holds a thread or a client's connection, which
/// is what this ceiling bounds. A run asked to take longer is stopped here.
///
/// CHOSEN, not cited: it COMPRESSES a configured timeout above it, so a revision configured
/// for longer than this gets this instead. A handler that WAITS is unaffected — the guest's
/// timers run on a virtual clock and spend none of it — so what this compresses is a handler
/// spending that long of this host's own CPU.
const RUN_WALL_CLOCK_CEILING: Duration = Duration::from_secs(10);

/// The smallest and largest `timeoutSeconds` the model allows (`@range(3..900)`), which
/// every stored value is clamped to before it reaches a run.
const TIMEOUT_SECONDS_RANGE: (i64, i64) = (3, 900);

/// How long the CALLER waits for a run before it is given up on: the run's own ceiling plus
/// the slack a stretch of engine-internal work can take past it.
///
/// A run still going when this passes is ABANDONED. The client is answered at once and the
/// serving slot ([`MAX_LIVE_RUNS`]) goes straight back, so a run nothing could stop does not
/// hold the slots requests are served from; the run's THREAD keeps a live-run count
/// ([`MAX_LIVE_RUN_THREADS`]) until it ends, which is what bounds how many abandoned runs
/// this process can hold at once, and the abandonment is recorded at warn level so a process
/// holding them is diagnosable rather than quietly refusing.
const CALLER_WAIT_CEILING: Duration = Duration::from_secs(15);

/// How many times the engine may ask whether this run should stop before it is stopped
/// anyway.
///
/// The engine asks as it executes guest code, so this is a ceiling on the WORK one request
/// may do at all, standing BEHIND the wall clock: it is set high enough that a handler
/// doing real work reaches its configured timeout first, and is there for a run the clock
/// cannot end on its own.
const RUN_ENGINE_CHECKS: u64 = 200_000_000;

/// What one run's engine may allocate. CHOSEN: the model publishes no memory size for a web
/// function, and a run is bounded all the same because the code is a caller's.
const RUN_MEMORY_BYTES: usize = 128 * 1024 * 1024;

/// The C stack one run's engine may use, well under [`RUN_THREAD_STACK_BYTES`], so deep
/// guest recursion ends that run with the engine's own stack-overflow error rather than
/// touching the thread's guard page.
const ENGINE_STACK_BYTES: usize = 512 * 1024;

/// The stack of the thread one run is given.
const RUN_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

/// How large a response one request may answer with. CHOSEN, not cited: the model publishes
/// no response ceiling, and a bound is needed because the body is the guest's. A response
/// over it is not served at all: a PREFIX of what the code wrote is not the response it
/// wrote, so the run is reported as having produced none.
pub const MAX_RESPONSE_BYTES: usize = 6 * 1024 * 1024;

/// How many log lines of one run are kept, and how long each is.
const MAX_LOG_ENTRIES: usize = 200;
/// See [`MAX_LOG_ENTRIES`].
const MAX_LOG_ENTRY_BYTES: usize = 4_096;

/// How many engine jobs (settled promises) one request may spend.
const MAX_PENDING_JOBS: usize = 20_000;

/// How many timer callbacks one request may spend.
const MAX_TIMER_TICKS: usize = 10_000;

/// How many web function runs this emulator SERVES at once, across every endpoint.
///
/// One permit per request being served, taken before a run is started and released when the
/// caller is answered — including where the run was given up on, so an abandoned run cannot
/// hold a slot a later request is served from. A request that finds no permit is answered
/// rather than queued.
pub const MAX_LIVE_RUNS: usize = 8;

/// How many run THREADS this process holds at once, the abandoned ones included.
///
/// Each live thread holds an engine, so this is the ceiling on what the runs in this process
/// hold at all: it stands above [`MAX_LIVE_RUNS`] to leave room for runs that were given up
/// on, and a request that finds every one of them live is answered rather than started.
pub const MAX_LIVE_RUN_THREADS: usize = 16;

/// The slots [`MAX_LIVE_RUNS`] bounds: ONE semaphore for the whole service, because a
/// caller chooses how many endpoints exist and each one would otherwise raise the peak
/// again.
fn run_slots() -> Arc<::tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<::tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(::tokio::sync::Semaphore::new(MAX_LIVE_RUNS))))
}

/// How many run THREADS this process holds, under the ceiling it admits them to.
///
/// Counted separately from the serving slots above, because the two bound different things:
/// a slot is held while a CALLER is being served, and a count here is held for as long as
/// the run's THREAD lives — which, for a run that was given up on, is longer. The count is
/// taken before the thread is started and released by the thread's own guard on every exit
/// path, so a run whose thread ends gives it back whatever the run came to and a run nothing
/// could stop holds exactly one.
#[derive(Debug)]
struct RunAdmission {
    /// How many threads are live.
    live: ::std::sync::atomic::AtomicUsize,
    /// How many may be.
    ceiling: usize,
}

impl RunAdmission {
    /// An admission of at most `ceiling` live run threads — the one constructor, so a
    /// ceiling small enough to fill in a test is filled through exactly the path a request's
    /// is.
    const fn new(ceiling: usize) -> Self {
        Self {
            live: ::std::sync::atomic::AtomicUsize::new(0),
            ceiling,
        }
    }

    /// Admit one run, or `None` when every thread this process holds at once is live.
    ///
    /// The count is raised and the ceiling tested in ONE atomic step, so two requests
    /// arriving together cannot both be admitted into the last place.
    fn admit(self: &Arc<Self>) -> Option<RunHeld> {
        let ceiling = self.ceiling;
        self.live
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                (live < ceiling).then(|| live + 1)
            })
            .ok()
            .map(|_| RunHeld {
                admission: Arc::clone(self),
            })
    }

    /// How many run threads are live.
    fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }
}

/// One admitted run's place in [`RunAdmission`], given back when it is dropped — which is
/// where the run's thread ends, however it ended.
#[derive(Debug)]
struct RunHeld {
    admission: Arc<RunAdmission>,
}

impl Drop for RunHeld {
    fn drop(&mut self) {
        let _ = self
            .admission
            .live
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                Some(live.saturating_sub(1))
            });
    }
}

/// The admission every run of this service is started under: ONE for the whole process, for
/// the same reason the slots above are one.
fn run_admission() -> Arc<RunAdmission> {
    static ADMISSION: OnceLock<Arc<RunAdmission>> = OnceLock::new();
    Arc::clone(ADMISSION.get_or_init(|| Arc::new(RunAdmission::new(MAX_LIVE_RUN_THREADS))))
}

/// One request a client sent to an endpoint's published address, as the guest is handed it.
#[derive(Debug, Clone)]
pub struct HandlerRequest {
    /// The HTTP method.
    pub method: String,
    /// The path and query the client addressed — `req.url` as a Node server reads it.
    pub url: String,
    /// The headers the client sent, lowercased as a Node server reports them.
    pub headers: Vec<(String, String)>,
    /// The request body, as the text the guest reads.
    pub body: String,
}

/// The response the guest answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerResponse {
    /// The status it set.
    pub status: u16,
    /// The headers it set, in the order it set them.
    pub headers: Vec<(String, String)>,
    /// The body it wrote.
    pub body: Vec<u8>,
}

/// What one request handed to a revision's code came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// The guest answered.
    Answered(HandlerResponse),
    /// The guest ran and produced no response: it registered no server, its code raised
    /// before answering, or it ended without answering. The clause says which.
    NoResponse(String),
    /// A bound of this module stopped the run. The clause says which.
    Stopped(String),
    /// Nothing of the guest ran at all — no slot was free, no thread could be started, or
    /// the engine did not start. Nothing was learned about the code.
    Unavailable(String),
}

/// How long the GUEST of a revision configured with `timeout_seconds` may run.
///
/// The stored value is clamped to the range the model allows before it is used, and then to
/// this module's own ceiling: a caller-chosen timeout decides when the guest is stopped and
/// never how long this process holds a thread.
#[must_use]
pub fn run_clock(timeout_seconds: i32) -> Duration {
    let (low, high) = TIMEOUT_SECONDS_RANGE;
    let seconds = i64::from(timeout_seconds).clamp(low, high);
    Duration::from_secs(seconds.unsigned_abs()).min(RUN_WALL_CLOCK_CEILING)
}

/// The prefix of every Node.js runtime identifier
/// (`RuntimeConfig$runtime`: "The runtime identifier for the web function (for example, a
/// Node.js runtime identifier)"), which is the family this module runs an engine for.
const NODE_RUNTIME_PREFIX: &str = "nodejs";

/// The release a Node.js runtime identifier names, as a three-part version, or `None` when
/// the identifier names no Node.js runtime at all.
///
/// Only what the identifier states is reported — `nodejs24.x` is the 24 line — because the
/// identifier is the only statement of a version there is.
#[must_use]
fn node_version(runtime: &str) -> Option<String> {
    let named = runtime.strip_prefix(NODE_RUNTIME_PREFIX)?;
    let mut parts: Vec<String> = Vec::new();
    for part in named.split('.') {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            break;
        }
        parts.push(digits);
    }
    match parts.len() {
        0 => None,
        1 => Some(format!("{}.0.0", parts[0])),
        _ => Some(format!("{}.{}.0", parts[0], parts[1])),
    }
}

/// Run `package` and hand it `request`, with `environment` the revision's own environment
/// variables, `timeout_seconds` its configured timeout and `runtime` the runtime identifier
/// its build configuration names.
///
/// The engine is built, the package's entry module is started, the request is handed to the
/// server that module registered, and what it answered is reported. The whole run happens
/// on a thread of its own: this future only waits for it, bounded by
/// [`CALLER_WAIT_CEILING`].
pub async fn serve_handler(
    package: Arc<DeploymentPackage>,
    environment: BTreeMap<String, String>,
    timeout_seconds: i32,
    runtime: String,
    request: HandlerRequest,
) -> HandlerOutcome {
    let Some(version) = node_version(&runtime) else {
        return HandlerOutcome::Unavailable(format!(
            "the revision names the runtime {runtime:?}, which this emulator runs no engine for"
        ));
    };
    // One permit per run being SERVED, held by this future: it goes back when the caller is
    // answered, on every path including the one that gives up on the run, so an abandoned
    // run cannot hold a slot a later request is served from.
    let Ok(serving) = Arc::clone(&run_slots()).try_acquire_owned() else {
        return HandlerOutcome::Unavailable(format!(
            "the {MAX_LIVE_RUNS} web function runs this emulator serves at once were all busy"
        ));
    };
    let _serving: ::tokio::sync::OwnedSemaphorePermit = serving;
    // ... and one place in the live-thread count, held by the run's own THREAD until it
    // ends: that is what bounds how many abandoned runs this process can hold at once.
    let admission = run_admission();
    let Some(held) = admission.admit() else {
        return HandlerOutcome::Unavailable(format!(
            "the {MAX_LIVE_RUN_THREADS} web function runs this emulator holds at once were all \
             live"
        ));
    };
    let (reply, answer) = ::tokio::sync::oneshot::channel();
    let clock = run_clock(timeout_seconds);
    let started = ::std::thread::Builder::new()
        .name("lambda-web-run".to_string())
        .stack_size(RUN_THREAD_STACK_BYTES)
        .spawn(move || {
            // Dropped where the thread ends, which is where its engine's heap is freed.
            let _held = held;
            let outcome = run(&package, &environment, &version, clock, &request);
            let _ = reply.send(outcome);
        });
    if started.is_err() {
        // `held` was moved into the closure the spawn did not take, so the place it held is
        // already back: a thread that never started holds nothing.
        return HandlerOutcome::Unavailable(
            "no thread could be started for the web function's code".to_string(),
        );
    }
    match ::tokio::time::timeout(CALLER_WAIT_CEILING, answer).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => {
            HandlerOutcome::NoResponse("the run ended without answering".to_string())
        }
        Err(_) => {
            // The run outlasted every bound the engine could land on it, so the caller is
            // answered without it. Recorded at warn level with what this process is holding,
            // because a run given up on keeps its place in the live-thread count until its
            // thread ends: a process accumulating them is diagnosable from these lines rather
            // than from later requests being refused.
            ::tracing::warn!(
                live_runs = admission.live(),
                ceiling = MAX_LIVE_RUN_THREADS,
                waited_seconds = CALLER_WAIT_CEILING.as_secs(),
                "a web function run was given up on and is still holding its place"
            );
            HandlerOutcome::Stopped(format!(
                "the run was given up on after {} seconds",
                CALLER_WAIT_CEILING.as_secs()
            ))
        }
    }
}

/// Build the engine, start the package in it and answer ONE request — the whole run, on the
/// thread it was given.
fn run(
    package: &DeploymentPackage,
    environment: &BTreeMap<String, String>,
    node_version: &str,
    clock: Duration,
    request: &HandlerRequest,
) -> HandlerOutcome {
    run_bounded(
        package,
        environment,
        node_version,
        Bounds::new(clock, RUN_ENGINE_CHECKS),
        request,
    )
}

/// [`run`] under the budget `bounds`: the ONE path a request's run takes, so a budget small
/// enough to spend in a test is spent exactly where a request's is.
fn run_bounded(
    package: &DeploymentPackage,
    environment: &BTreeMap<String, String>,
    node_version: &str,
    bounds: Bounds,
    request: &HandlerRequest,
) -> HandlerOutcome {
    if package.is_empty() {
        return HandlerOutcome::NoResponse(
            "the revision's deployment package holds no module to start".to_string(),
        );
    }
    let bounds = Arc::new(bounds);
    let seams = Seams::new(Arc::clone(&bounds));
    let Ok(runtime) = Runtime::new() else {
        return HandlerOutcome::Unavailable("the engine did not start".to_string());
    };
    runtime.set_memory_limit(RUN_MEMORY_BYTES);
    runtime.set_max_stack_size(ENGINE_STACK_BYTES);
    // The only thing that can stop a guest with no exit: a timeout cannot preempt a loop
    // running on this thread, but the engine asks this handler as it works.
    let watched = Arc::clone(&bounds);
    runtime.set_interrupt_handler(Some(Box::new(move || watched.spent())));
    runtime.set_loader(RuntimeResolver, RuntimeLoader);
    let Ok(context) = Context::full(&runtime) else {
        runtime.set_interrupt_handler(None);
        return HandlerOutcome::Unavailable("the engine did not start".to_string());
    };

    let outcome = answer_in(
        &runtime,
        &context,
        &seams,
        package,
        environment,
        node_version,
        request,
    );
    // The run is over; nothing the engine does while it is being torn down is the guest's
    // work, and nothing is in flight to stop.
    runtime.set_interrupt_handler(None);
    for line in seams.logs() {
        // The guest's own output, rendered in its escaped form so a line it wrote cannot
        // forge a log line of this emulator's.
        ::tracing::debug!(line = ?line, "a web function wrote a log line");
    }
    outcome
}

/// Start the package and hand it the request, inside an engine that is already built and
/// bounded.
fn answer_in(
    runtime: &Runtime,
    context: &Context,
    seams: &Seams,
    package: &DeploymentPackage,
    environment: &BTreeMap<String, String>,
    node_version: &str,
    request: &HandlerRequest,
) -> HandlerOutcome {
    let started =
        context.with(|ctx| start_guest(&ctx, seams, package, environment, node_version));
    if let Err(outcome) = started {
        // A breached bound is the run's answer, even where the engine's own interruption is
        // what the package's code saw: the latch is host state the guest cannot reach.
        if let Some(breach) = seams.bounds.breach() {
            return HandlerOutcome::Stopped(breach);
        }
        return outcome;
    }
    // A package whose top-level code is still settling promises is driven through them, so
    // a server registered from one is listening by the time the request arrives.
    drive_jobs(runtime, &seams.bounds);
    if let Some(breach) = seams.bounds.breach() {
        return HandlerOutcome::Stopped(breach);
    }

    let dispatched = context.with(|ctx| {
        let runtime_object: Object<'_> = match ctx.globals().get(node_runtime::RUNTIME_GLOBAL) {
            Ok(object) => object,
            Err(error) => return Err(error.to_string()),
        };
        let dispatch: Function<'_> = match runtime_object.get("dispatch") {
            Ok(dispatch) => dispatch,
            Err(error) => return Err(error.to_string()),
        };
        let headers =
            ::serde_json::to_string(&request.headers).unwrap_or_else(|_| "[]".to_string());
        dispatch
            .call::<_, bool>((
                request.method.clone(),
                request.url.clone(),
                headers,
                request.body.clone(),
            ))
            .catch(&ctx)
            .map_err(|caught| raised_message(&caught))
    });
    if dispatched == Ok(false) {
        return HandlerOutcome::NoResponse(NO_LISTENER.to_string());
    }
    let raised = dispatched.err();
    // A guest that has not answered yet is still working: its promises are settled by the
    // engine's jobs and its callbacks by its own timers, and nothing but this drives them.
    if seams.answer().is_none() {
        drain(runtime, context, &seams.bounds, &seams.answered);
    }
    context.with(|ctx| drop(ctx.catch()));

    // A breached bound is the request's answer, whatever the guest then said: the latch is
    // host state the guest cannot reach.
    if let Some(breach) = seams.bounds.breach() {
        return HandlerOutcome::Stopped(breach);
    }
    match seams.answer() {
        Some(answer) => HandlerOutcome::Answered(answer),
        None => HandlerOutcome::NoResponse(match (seams.refusal(), raised) {
            // A response the host could not serve is reported as itself, before anything the
            // code went on to raise.
            (Some(refusal), _) => refusal,
            (None, Some(raised)) => {
                format!("the web function raised before it answered: {raised}")
            }
            (None, None) => NO_RESPONSE.to_string(),
        }),
    }
}

/// What a package that registered no server is reported as.
const NO_LISTENER: &str = "the web function registered no HTTP server, so nothing is listening";

/// What a package that registered a server and never answered is reported as.
const NO_RESPONSE: &str = "the web function did not answer the request";

/// Install the host functions, the runtime and the package, and run the package's entry
/// module.
///
/// The error is what the request is answered with: a runtime that could not be installed is
/// this emulator's own fault, while a package that raises or holds no entry module is a
/// fact about the revision's code.
fn start_guest(
    ctx: &Ctx<'_>,
    seams: &Seams,
    package: &DeploymentPackage,
    environment: &BTreeMap<String, String>,
    node_version: &str,
) -> Result<(), HandlerOutcome> {
    install_host_seams(ctx, seams)
        .map_err(|reason| HandlerOutcome::Unavailable(format!("the engine did not start: {reason}")))?;
    // Held BEFORE any of the guest's code is compiled, so the runtime its request is driven
    // through is this module's own however the guest rebinds the global object. It is called
    // with the SHARED module registry and the SHARED pure core modules
    // ([`::service_common::node_guest_modules`]): Node's resolution and its pure standard
    // library are the same for every service that runs Node.js code, so this runtime
    // compiles that source rather than a copy of its own.
    let core: ::std::collections::BTreeMap<&str, &str> =
        ::service_common::node_guest_modules::PURE_CORE_MODULES
            .iter()
            .copied()
            .collect();
    let built = format!(
        "({runtime})({registry}, {core})",
        runtime = node_runtime::RUNTIME_SOURCE,
        registry = ::serde_json::to_string(
            ::service_common::node_guest_modules::MODULE_REGISTRY_SOURCE
        )
        .unwrap_or_else(|_| "\"\"".to_string()),
        core = ::serde_json::to_string(&core).unwrap_or_else(|_| "{}".to_string()),
    );
    let runtime_object: Object<'_> = ctx
        .eval::<Object<'_>, _>(built)
        .catch(ctx)
        .map_err(|caught| {
            HandlerOutcome::Unavailable(format!(
                "the web function runtime could not be installed: {caught}"
            ))
        })?;
    ctx.globals()
        .set(node_runtime::RUNTIME_GLOBAL, runtime_object.clone())
        .map_err(|error| {
            HandlerOutcome::Unavailable(format!(
                "the web function runtime could not be installed: {error}"
            ))
        })?;

    let files = ::serde_json::to_string(&package.files).unwrap_or_else(|_| "{}".to_string());
    let variables = ::serde_json::to_string(environment).unwrap_or_else(|_| "{}".to_string());
    // The release the runtime identifier names, which is what the guest's `process.version`
    // reports: the identifier is the only statement of a version there is.
    if let Ok(set_version) = runtime_object.get::<_, Function<'_>>("setNodeVersion") {
        let _ = set_version.call::<_, ()>((node_version.to_string(),));
    }
    let start: Function<'_> = runtime_object.get("start").map_err(|error| {
        HandlerOutcome::Unavailable(format!("the web function runtime has no entry point: {error}"))
    })?;
    // What the registry resolved as the package's entry module, and whether it is an ES
    // module: the two questions Node's own resolution answers, asked of the registry that
    // holds the package's files.
    let entry: String = start
        .call((files, variables, node_runtime::MODULE_ACCESSOR.to_string()))
        .catch(ctx)
        .map_err(|caught| {
            HandlerOutcome::Unavailable(format!(
                "the web function runtime could not read the package: {caught}"
            ))
        })?;
    let entry: ::serde_json::Value = ::serde_json::from_str(&entry).unwrap_or_default();
    let path = entry
        .get("entry")
        .and_then(::serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if path.is_empty() {
        return Err(HandlerOutcome::NoResponse(
            "the revision's deployment package holds no module to start".to_string(),
        ));
    }
    let module = entry
        .get("module")
        .and_then(::serde_json::Value::as_bool)
        .unwrap_or_default();
    let source = entry
        .get("source")
        .and_then(::serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();

    // From here on what runs is the GUEST's own code, started the way Node starts the
    // package's entry point: as an ES module where the package says so, and as a CommonJS
    // module otherwise.
    if module {
        return evaluate_module(ctx, &path, &source);
    }
    let require_entry: Function<'_> = runtime_object.get("requireEntry").map_err(|error| {
        HandlerOutcome::Unavailable(format!("the web function runtime has no loader: {error}"))
    })?;
    require_entry
        .call::<_, ()>((path,))
        .catch(ctx)
        .map_err(|caught| {
            HandlerOutcome::NoResponse(format!(
                "the web function raised before it answered: {}",
                raised_message(&caught)
            ))
        })
}

/// Compile and evaluate one ES module of the package as its entry point.
fn evaluate_module(ctx: &Ctx<'_>, path: &str, source: &str) -> Result<(), HandlerOutcome> {
    let declared = Module::declare(ctx.clone(), path.to_string(), source.to_string())
        .catch(ctx)
        .map_err(|caught| {
            HandlerOutcome::NoResponse(format!(
                "the web function raised before it answered: {}",
                raised_message(&caught)
            ))
        })?;
    let (_, evaluation) = declared.eval().catch(ctx).map_err(|caught| {
        HandlerOutcome::NoResponse(format!(
            "the web function raised before it answered: {}",
            raised_message(&caught)
        ))
    })?;
    // A module's evaluation answers a PROMISE, so an exception its body threw is that
    // promise's rejection rather than this call's error. A pending evaluation is settled by
    // the jobs the caller drives next.
    let finished: ::rquickjs::Result<()> = evaluation.finish();
    if matches!(finished, Err(::rquickjs::Error::WouldBlock)) {
        return Ok(());
    }
    finished.catch(ctx).map_err(|caught| {
        HandlerOutcome::NoResponse(format!(
            "the web function raised before it answered: {}",
            raised_message(&caught)
        ))
    })
}

/// Settle the engine's pending jobs, under the bounds of the work in flight.
fn drive_jobs(runtime: &Runtime, bounds: &Arc<Bounds>) {
    let mut jobs = 0usize;
    while jobs < MAX_PENDING_JOBS {
        match runtime.execute_pending_job() {
            Ok(true) => jobs += 1,
            Ok(false) | Err(_) => break,
        }
        if bounds.stop_if_due() {
            break;
        }
    }
}

/// Drive the guest until it answers, until it has nothing left to do, or until a bound ends
/// the run: the engine's jobs first, then the earliest timer callback the guest's own clock
/// has due.
fn drain(
    runtime: &Runtime,
    context: &Context,
    bounds: &Arc<Bounds>,
    answered: &Arc<Mutex<Option<HandlerResponse>>>,
) {
    let mut jobs = 0usize;
    let mut ticks = 0usize;
    loop {
        if bounds.stop_if_due() {
            return;
        }
        while jobs < MAX_PENDING_JOBS {
            match runtime.execute_pending_job() {
                Ok(true) => jobs += 1,
                Ok(false) | Err(_) => break,
            }
            if bounds.stop_if_due() {
                return;
            }
        }
        if answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return;
        }
        if ticks >= MAX_TIMER_TICKS || jobs >= MAX_PENDING_JOBS {
            return;
        }
        ticks += 1;
        let more = context.with(|ctx| {
            ctx.globals()
                .get::<_, Object<'_>>(node_runtime::RUNTIME_GLOBAL)
                .and_then(|runtime_object| runtime_object.get::<_, Function<'_>>("tick"))
                .and_then(|tick| tick.call::<_, bool>(()))
                .unwrap_or(false)
        });
        if !more {
            return;
        }
    }
}

/// Install the host functions the guest's runtime is built over.
///
/// They are the only host functions reachable from inside the engine, and the runtime
/// source deletes their global bindings as soon as it holds them, so after it has run they
/// are reachable from nothing but its own closures. Neither of them opens, resolves or
/// reads anything of the host: one latches the response, the other collects a log line.
fn install_host_seams(ctx: &Ctx<'_>, seams: &Seams) -> Result<(), String> {
    let latch = Arc::clone(&seams.answered);
    let refused = Arc::clone(&seams.refused);
    let respond = Func::from(move |status: i32, headers: String, body: String| {
        let mut held = latch.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_some() {
            // A guest that answers twice has answered once: the first response is the one
            // the client reads, exactly as a server that has already flushed its headers
            // cannot take them back.
            return;
        }
        let body = body.into_bytes();
        if body.len() > MAX_RESPONSE_BYTES {
            // Over the ceiling: nothing is latched, so the run reports the response it could
            // not serve rather than the part of it that fitted.
            *refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!(
                "the web function answered with more than the {MAX_RESPONSE_BYTES} bytes a \
                 response may carry"
            ));
            return;
        }
        *held = Some(HandlerResponse {
            status: u16::try_from(status).unwrap_or(200),
            headers: ::serde_json::from_str::<Vec<(String, String)>>(&headers).unwrap_or_default(),
            body,
        });
    });

    let logged = Arc::clone(&seams.collected);
    let log = Func::from(move |message: String| {
        logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .log(&message);
    });

    let globals = ctx.globals();
    globals
        .set(node_runtime::HOST_RESPOND, respond)
        .and_then(|()| globals.set(node_runtime::HOST_LOG, log))
        .map_err(|error| error.to_string())
}

/// The message a raise carries: the error's own, as the guest's code worded it.
fn raised_message(caught: &::rquickjs::CaughtError<'_>) -> String {
    match caught {
        ::rquickjs::CaughtError::Exception(exception) => exception
            .message()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| exception.to_string()),
        other => other.to_string(),
    }
}

/// How an `import` specifier is resolved: by the runtime's own registry, which answers with
/// a file of this package or a core module this emulator implements, and with nothing else.
#[derive(Debug, Clone, Copy)]
struct RuntimeResolver;

impl Resolver for RuntimeResolver {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> ::rquickjs::Result<String> {
        let resolved: Option<String> =
            runtime_call(ctx, "resolveModule", (base.to_string(), name.to_string()))
                .ok()
                .flatten();
        resolved.ok_or_else(|| ::rquickjs::Error::new_resolving(base.to_string(), name.to_string()))
    }
}

/// How a resolved module is loaded: from the source the runtime hands back for it. No host
/// path is opened for a module, because there is no module outside this package and this
/// emulator's own core modules.
#[derive(Debug, Clone, Copy)]
struct RuntimeLoader;

impl Loader for RuntimeLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        path: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> ::rquickjs::Result<Module<'js>> {
        let source: Option<String> = runtime_call(ctx, "moduleSource", (path.to_string(),))
            .map_err(|_| ::rquickjs::Error::new_loading(path.to_string()))?;
        let Some(source) = source else {
            return Err(::rquickjs::Error::new_loading(path.to_string()));
        };
        Module::declare(ctx.clone(), path.to_string(), source)
    }
}

/// Call one function of the runtime's own protocol object.
fn runtime_call<'js, A, R>(ctx: &Ctx<'js>, name: &str, args: A) -> Result<R, String>
where
    A: ::rquickjs::function::IntoArgs<'js>,
    R: ::rquickjs::FromJs<'js>,
{
    let runtime: Object<'js> = ctx
        .globals()
        .get(node_runtime::RUNTIME_GLOBAL)
        .map_err(|error| format!("the web function runtime did not install itself: {error}"))?;
    let function: Function<'js> = runtime
        .get(name)
        .map_err(|error| format!("the web function runtime has no {name}: {error}"))?;
    function
        .call(args)
        .catch(ctx)
        .map_err(|caught| format!("the web function runtime's {name} failed: {caught}"))
}

/// The per-request seams the guest's runtime is built over: the host state the run writes
/// into, and the budget it is held to.
struct Seams {
    /// The response the guest answered with.
    answered: Arc<Mutex<Option<HandlerResponse>>>,
    /// Why a response the guest wrote could not be served at all, when one could not.
    refused: Arc<Mutex<Option<String>>>,
    /// The log lines it wrote.
    collected: Arc<Mutex<Collected>>,
    /// The budget the run is held to.
    bounds: Arc<Bounds>,
}

impl Seams {
    /// The seams of one run.
    fn new(bounds: Arc<Bounds>) -> Self {
        Self {
            answered: Arc::new(Mutex::new(None)),
            refused: Arc::new(Mutex::new(None)),
            collected: Arc::new(Mutex::new(Collected::default())),
            bounds,
        }
    }

    /// What the guest answered with, if it has answered.
    fn answer(&self) -> Option<HandlerResponse> {
        self.answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Why what the guest wrote could not be served, when it could not.
    fn refusal(&self) -> Option<String> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The lines the guest wrote.
    fn logs(&self) -> Vec<String> {
        self.collected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .logs
            .clone()
    }
}

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
            let boundary = line
                .char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index <= MAX_LOG_ENTRY_BYTES)
                .last()
                .unwrap_or_default();
            line.truncate(boundary);
        }
        self.logs.push(line);
    }
}

/// The budget one REQUEST is held to, as the engine's interrupt handler spends it, and the
/// latch that says which bound ended it.
#[derive(Debug)]
struct Bounds {
    /// The instant this budget measures its clock from.
    origin: Instant,
    /// How long the run may take.
    wall_clock: Duration,
    /// How many engine checks the run started with, which a breach reports.
    budget: u64,
    /// How many engine checks are left.
    checks: AtomicU64,
    /// Set once the clock ran out.
    timed_out: AtomicBool,
    /// Set once the checks ran out.
    steps_spent: AtomicBool,
}

impl Bounds {
    /// A budget of `budget` engine checks and `wall_clock` of the host's clock, starting
    /// now — the one constructor, so a budget small enough to spend in a test is spent
    /// through exactly the path a request's is.
    fn new(wall_clock: Duration, budget: u64) -> Self {
        Self {
            origin: Instant::now(),
            wall_clock,
            budget,
            checks: AtomicU64::new(budget),
            timed_out: AtomicBool::new(false),
            steps_spent: AtomicBool::new(false),
        }
    }

    /// Whether the run is over, asked by the engine as it works: the latch first, then the
    /// clock, then the step budget. Each answer latches, so a breach survives a guest that
    /// catches the exception the engine raises in it.
    fn spent(&self) -> bool {
        if self.steps_spent.load(Ordering::Relaxed) {
            return true;
        }
        if self.stop_if_due() {
            return true;
        }
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
        if self.timed_out.load(Ordering::Relaxed) {
            return true;
        }
        if self.steps_spent.load(Ordering::Relaxed) {
            return true;
        }
        if self.origin.elapsed() >= self.wall_clock {
            self.timed_out.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// What the breached bound is reported as, or `None` when none was breached.
    fn breach(&self) -> Option<String> {
        if self.timed_out.load(Ordering::Relaxed) {
            return Some(format!(
                "the web function was stopped after {} seconds without answering",
                self.wall_clock.as_secs()
            ));
        }
        if self.steps_spent.load(Ordering::Relaxed) {
            return Some(format!(
                "the web function was stopped after {} engine steps without answering",
                self.budget
            ));
        }
        None
    }
}
