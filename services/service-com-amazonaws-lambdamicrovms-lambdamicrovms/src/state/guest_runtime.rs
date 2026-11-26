// SPDX-License-Identifier: CC0-1.0

//! RUNNING a MicroVM's guest: the engine the image's own code executes in, inside this
//! process, and the one request a published endpoint hands it.
//!
//! `RunMicrovmResponse$endpoint` is "the HTTPS endpoint URL for communicating with the
//! MicroVM", and what a client reads back from it is the GUEST's own HTTP response —
//! whatever the guest chose to send. None of that can be assembled here, so none of it
//! is: the guest's program ([`super::guest_program`]) is compiled and executed by the
//! QuickJS engine embedded in this process (`rquickjs`), the server it registers is
//! handed the request the client sent, and the status, headers and body it answered are
//! read back off that run. A guest that registers no server, or whose code raises before
//! it answers, produces no response, and that is reported as what it is.
//!
//! # What the guest can reach
//!
//! ONE engine per MicroVM, started where its guest process is ([`GuestProcess`]). Inside
//! it the guest reaches the language, the modules this module implements for it
//! ([`RUNTIME_SOURCE`]: `http`, `net`, `fs`, `process`, `os`, `path`, `url`, `events`,
//! `querystring`, a console and timers) and the host functions those are built over — the
//! response sink, the outbound-connection seam, the log writer and the seams its MACHINE
//! answers ([`super::guest_machine`]) — each deleted from the global object before any of
//! the guest's own code is compiled, so only this module's closures still hold them. No
//! module loader is installed, so an `import` resolves to nothing; plain QuickJS registers
//! no filesystem, process, environment, socket, host-library or thread API, and the ones
//! here are the MICROVM's rather than this machine's: the guest's `fs` reads and writes
//! the in-memory tree its MicroVM holds and opens no host path, its `process` reports the
//! identity and the environment its MicroVM published to it and holds nothing of this
//! emulator's own process, and its `net` opens no host socket — where a connection goes is
//! decided by the MicroVM's egress network connectors ([`super::microvm_network`]). No
//! child-process, host-library or thread module exists at all.
//!
//! # What bounds it
//!
//! Every bound lives in host code the guest cannot reach:
//!
//! * a WALL CLOCK ([`RUN_WALL_CLOCK`]) and a STEP budget ([`RUN_ENGINE_CHECKS`]), both
//!   spent by the engine's own interrupt handler as it works, so a guest that never
//!   returns and one that loops without allocating are each stopped;
//! * a MEMORY ceiling ([`RUN_MEMORY_BYTES`]) and a JavaScript STACK ceiling
//!   ([`RUN_STACK_BYTES`]), both the engine's own;
//! * ceilings on what one request may produce: the response body
//!   ([`MAX_RESPONSE_BYTES`]), the log lines ([`MAX_LOG_ENTRIES`],
//!   [`MAX_LOG_ENTRY_BYTES`]) and the outbound connections one request may open
//!   ([`MAX_CONNECTS`]);
//! * bounds on the work the run is driven through: the engine jobs
//!   ([`MAX_PENDING_JOBS`]) and the timer callbacks ([`MAX_TIMER_TICKS`]) one request may
//!   spend;
//! * a LIVE-PROCESS ceiling ([`MAX_LIVE_GUESTS`]), a QUEUE ceiling
//!   ([`MAX_QUEUED_REQUESTS`]) and a CALLER WAIT ceiling ([`CALLER_WAIT_CEILING`]) held
//!   outside the engine, so however many MicroVMs a client runs and however many requests
//!   it sends at once, what the engines hold is bounded and each request is answered even
//!   where a stretch of engine-internal work outlasts the run's own clock;
//! * the bounds its MACHINE holds on what the guest's writes keep
//!   ([`super::guest_machine::volume_limits`]).
//!
//! Which bound was breached is LATCHED in host state rather than read back out of the
//! engine, so a guest that catches the exception the engine raises in it cannot make the
//! run report a response.
//!
//! # The guest's clock
//!
//! A guest's timers run on a VIRTUAL clock: `setTimeout` records when a callback is due
//! in the guest's own time, and the host runs the earliest due callback whenever nothing
//! else can make progress. Nothing sleeps — a guest that waits five seconds for a
//! connection costs the client no wall-clock time — and the ORDER the guest sees is the
//! order it asked for.
//!
//! The run happens on a BLOCKING thread, never on the async executor serving the request,
//! and no store lock is held across it.
//!
//! # The guest PROCESS
//!
//! The engine is NOT rebuilt per request: a MicroVM runs its guest, so the program is
//! started ONCE, on a thread of its own ([`GuestProcess`]), and every request the
//! endpoint proxies is handed to that same engine. What the guest holds between two
//! requests is therefore what a process inside a MicroVM holds — the variables its
//! modules keep, the server it registered, the files it wrote to its machine
//! ([`super::guest_machine`]) — which is what `SuspendMicrovm` "preserving its full
//! memory and disk state" and `ResumeMicrovm` "restoring it … with all state intact"
//! describe. The process lives until the MicroVM stops holding it; dropping it closes the
//! call channel, which is what ends its thread and frees the engine's heap.
//!
//! One process answers one request at a time, as a guest with one vCPU does, and every
//! bound below is spent per REQUEST rather than once for the life of the process.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::rquickjs::function::Func;
use ::rquickjs::{CatchResultExt, Context, Ctx, Function, Object, Runtime};

use super::guest_machine::GuestMachine;
use super::guest_program::{GuestLanguage, GuestProgram, GuestStart};
use super::microvm_network::ConnectVerdict;
use super::ComAmazonawsLambdamicrovmsLambdamicrovmsState as ServiceState;

/// How long one request's run may take before the engine's interrupt handler stops it.
const RUN_WALL_CLOCK: Duration = Duration::from_secs(5);

/// How long the CALLER waits for a run at most, which is longer than the run's own clock
/// by the margin it takes the engine to notice that clock.
const CALLER_WAIT_CEILING: Duration = Duration::from_secs(10);

/// How many engine checks one run may spend — the STEP bound beside the wall-clock one,
/// and the one that does not depend on how fast the host is.
const RUN_ENGINE_CHECKS: u64 = 2_000_000;

/// The heap one run's engine is built with.
const RUN_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// The JavaScript stack one run's engine is built with, well inside the stack of the
/// blocking thread the run happens on.
const RUN_STACK_BYTES: usize = 512 * 1024;

/// How large a response body one request may carry.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// How many log lines one request may write.
const MAX_LOG_ENTRIES: usize = 200;

/// How long one log line may be, in bytes.
const MAX_LOG_ENTRY_BYTES: usize = 4_096;

/// How many outbound connections one request may open.
pub(super) const MAX_CONNECTS: u64 = 16;

/// How many engine jobs one request may spend.
const MAX_PENDING_JOBS: usize = 20_000;

/// How many timer callbacks one request may spend.
const MAX_TIMER_TICKS: usize = 10_000;

/// How many guest PROCESSES this emulator runs at once: each holds a thread and the
/// engine's own heap for as long as its MicroVM does, so this is what bounds what those
/// processes occupy however many MicroVMs a client runs.
///
/// It is a HARD ceiling, not a store ceiling: the slot is taken before a process starts
/// and released where its engine is freed ([`guest_slots`]), so the engines and threads
/// alive at any instant cannot exceed it even where a request still holds a process the
/// store has let go of. A request that finds no slot is answered as one the guest never
/// saw ([`super::ComAmazonawsLambdamicrovmsLambdamicrovmsState::guest_process`]).
pub const MAX_LIVE_GUESTS: usize = 8;

/// The live-process slots [`MAX_LIVE_GUESTS`] declares, over every MicroVM of this
/// process.
fn guest_slots() -> &'static Arc<::tokio::sync::Semaphore> {
    static SLOTS: ::std::sync::OnceLock<Arc<::tokio::sync::Semaphore>> =
        ::std::sync::OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(::tokio::sync::Semaphore::new(MAX_LIVE_GUESTS)))
}

/// The live-process slot ONE guest thread holds out of [`MAX_LIVE_GUESTS`].
///
/// It is held here rather than moved into the thread, so the host can take it back from
/// OUTSIDE that thread: a guest the host has GIVEN UP on — one still inside a call of its
/// engine after its budget was spent, which only that engine can leave
/// ([`super::guest_python`]) — stops holding the room a guest that answers could use, while
/// the thread it is on is counted as given up on for as long as it lasts. Dropping every
/// handle releases the slot too, which is what happens when the thread ends.
#[derive(Debug, Clone)]
pub(super) struct GuestSlot {
    held: Arc<Mutex<Option<::tokio::sync::OwnedSemaphorePermit>>>,
}

impl GuestSlot {
    /// Take one of this emulator's live-process slots, or `None` where every one is taken.
    fn take() -> Option<Self> {
        let permit = Arc::clone(guest_slots()).try_acquire_owned().ok()?;
        Some(Self {
            held: Arc::new(Mutex::new(Some(permit))),
        })
    }

    /// Give the slot back at once. Idempotent: a slot already given back stays given back.
    pub(super) fn release(&self) {
        drop(
            self.held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }
}

/// The host stack the engine's own thread is given, well outside the JavaScript stack
/// ([`RUN_STACK_BYTES`]) the engine is built with.
const GUEST_THREAD_STACK_BYTES: usize = 4 * 1024 * 1024;

/// How many requests may WAIT for one guest process at once. The engine answers one at a
/// time, so this is what bounds the queue however many clients reach one MicroVM: past it
/// a request is answered as one the guest never saw rather than held.
const MAX_QUEUED_REQUESTS: usize = 64;

/// How long the host waits for one outbound-connection decision before the connection is
/// reported as one nothing carried.
pub(super) const CONNECT_DECISION_CEILING: Duration = Duration::from_secs(5);

/// One request a client sent through a MicroVM's published endpoint, as the guest is
/// handed it.
#[derive(Debug, Clone)]
pub struct GuestRequest {
    /// The PORT of the MicroVM the request addresses, which is the port of the guest's
    /// own server it is handed to.
    pub port: i32,
    /// The HTTP method.
    pub method: String,
    /// The path and query the client addressed the guest with — `req.url` as a Node
    /// server reads it.
    pub url: String,
    /// The headers the client sent, lowercased as a Node server reports them.
    pub headers: Vec<(String, String)>,
    /// The request body, as the text the guest reads.
    pub body: String,
}

/// The response a guest answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestAnswer {
    /// The status the guest set.
    pub status: u16,
    /// The headers it set, in the order it set them.
    pub headers: Vec<(String, String)>,
    /// The body it wrote.
    pub body: Vec<u8>,
}

/// What one proxied request came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestOutcome {
    /// The guest answered.
    Answered(GuestAnswer),
    /// The MicroVM's image holds no guest program this engine can run, so nothing is
    /// listening inside the MicroVM.
    NoGuest,
    /// A guest ran and produced no response: it registered no server, its code raised
    /// before answering, or a bound of this module stopped it. The clause says which.
    NoResponse(String),
    /// Nothing of the guest ran at all — the engine did not start, or every run slot was
    /// taken. Nothing was learned about the guest.
    Unavailable(String),
}

/// One outbound connection a guest opened, handed to the host to decide.
#[derive(Debug)]
pub struct ConnectAsk {
    /// The address the guest named.
    pub host: String,
    /// The port it named.
    pub port: i32,
    /// Where the decision is sent back to the run that is waiting for it.
    pub reply: ::std::sync::mpsc::Sender<ConnectVerdict>,
}

/// The channel a run's outbound connections are decided over: the async side of the
/// listener holds the receiver and answers each ask from the emulated network.
pub type ConnectSeam = ::tokio::sync::mpsc::UnboundedSender<ConnectAsk>;

/// One request handed to a guest process, and where its outcome goes back to.
struct Call {
    /// The request the client sent.
    request: GuestRequest,
    /// The seam THIS request's outbound connections are decided over: the async side of
    /// the listener serving it holds the receiver.
    connects: ConnectSeam,
    /// Where the outcome is sent back to the caller waiting for it.
    reply: ::tokio::sync::oneshot::Sender<GuestOutcome>,
}

/// The guest PROCESS of one MicroVM: its program, started once, in an engine kept alive
/// on a thread of its own.
///
/// Dropping it closes the call channel, which is what ends that thread and frees the
/// engine's heap — so a MicroVM stops holding a process exactly when its record stops
/// holding this.
#[derive(Debug)]
pub struct GuestProcess {
    calls: ::tokio::sync::mpsc::Sender<Call>,
    /// The ports the guest's own servers bound, published by the engine's thread once the
    /// program has started and `None` until then. What the endpoint reads the port it
    /// FRONTS from ([`Self::fronted_port`]).
    ports: ::tokio::sync::watch::Receiver<Option<Vec<i32>>>,
}

impl GuestProcess {
    /// START `program` on `machine`: the engine is built, the program's entry module is
    /// run and the server it registers is kept, all on the process's own thread.
    ///
    /// `None` where every one of the [`MAX_LIVE_GUESTS`] slots this emulator holds is
    /// taken, or where no thread could be started at all — each of which leaves the caller
    /// to report a MicroVM it reached nothing inside of rather than a guest that answered.
    pub fn boot(program: Arc<GuestProgram>, machine: Arc<GuestMachine>) -> Option<Self> {
        // The slot is held by the PROCESS and dropped where its engine is freed, so it goes
        // back when the thread ends — and the host can give it back earlier for a guest it
        // has given up on ([`GuestSlot`]).
        let slot = GuestSlot::take()?;
        let held = slot.clone();
        let (calls, mut queue) = ::tokio::sync::mpsc::channel::<Call>(MAX_QUEUED_REQUESTS);
        let (bound, ports) = ::tokio::sync::watch::channel(None);
        ::std::thread::Builder::new()
            .name("microvm-guest".to_string())
            .stack_size(GUEST_THREAD_STACK_BYTES)
            .spawn(move || {
                let _slot = held;
                serve_calls(&program, &machine, &slot, &mut queue, &bound);
            })
            .ok()?;
        Some(Self { calls, ports })
    }

    /// The port this MicroVM's endpoint FRONTS: the first port the guest's own servers
    /// bound, and `None` where the guest bound no port of its own (one that registered no
    /// server, or one listening without naming a port).
    ///
    /// Awaits the start of the guest's program, which is what knows the port — bounded,
    /// like every wait on a guest, by [`CALLER_WAIT_CEILING`], past which the caller is
    /// answered rather than left hanging.
    pub async fn fronted_port(&self) -> Option<i32> {
        let mut ports = self.ports.clone();
        let reported = ::tokio::time::timeout(
            CALLER_WAIT_CEILING,
            ports.wait_for(|bound| bound.is_some()),
        )
        .await;
        match reported {
            Ok(Ok(bound)) => bound
                .clone()
                .unwrap_or_default()
                .into_iter()
                .find(|port| *port > 0),
            _ => None,
        }
    }

    /// Whether the process is still there to be asked.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.calls.is_closed()
    }

    /// Hand `request` to this guest and report what it answered, with `connects` the seam
    /// the connections it opens while answering are decided over.
    ///
    /// The engine answers one request at a time, as a guest with one vCPU does, so a
    /// request may WAIT for the one ahead of it — bounded, like the run itself, by
    /// [`CALLER_WAIT_CEILING`], past which the client is answered rather than left
    /// hanging.
    pub async fn serve(&self, request: GuestRequest, connects: ConnectSeam) -> GuestOutcome {
        let (reply, answer) = ::tokio::sync::oneshot::channel();
        // Queued rather than awaited: a queue at its bound is reported, so a client is
        // never left holding a request this process has no room for.
        if let Err(full) = self.calls.try_send(Call {
            request,
            connects,
            reply,
        }) {
            return match full {
                ::tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    GuestOutcome::Unavailable(format!(
                        "the {MAX_QUEUED_REQUESTS} requests this MicroVM's guest process \
                         queues at once were all waiting"
                    ))
                }
                ::tokio::sync::mpsc::error::TrySendError::Closed(_) => GuestOutcome::Unavailable(
                    "the MicroVM's guest process is no longer running".to_string(),
                ),
            };
        }
        match ::tokio::time::timeout(CALLER_WAIT_CEILING, answer).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => GuestOutcome::Unavailable(
                "the MicroVM's guest process ended without answering".to_string(),
            ),
            Err(_) => GuestOutcome::Unavailable(format!(
                "the guest was given up on after {} seconds",
                CALLER_WAIT_CEILING.as_secs()
            )),
        }
    }
}

/// The guest process's own thread: build the engine, start the program in it, and answer
/// every call the endpoint sends until the channel closes.
///
/// Everything the engine holds lives in this frame, so the engine is freed exactly where
/// the loop ends.
fn serve_calls(
    program: &GuestProgram,
    machine: &Arc<GuestMachine>,
    slot: &GuestSlot,
    queue: &mut ::tokio::sync::mpsc::Receiver<Call>,
    bound: &::tokio::sync::watch::Sender<Option<Vec<i32>>>,
) {
    let seams = Seams::new(Arc::clone(machine));
    let started = start_guest(program, machine, slot, &seams);
    // What the guest bound is published whichever way its program started, so nothing waits
    // on a process that started no server at all.
    let _ = bound.send(Some(started.ports()));
    while let Some(call) = queue.blocking_recv() {
        let outcome = match &started {
            Guest::Engine(engine) => engine.answer(&seams, &call.request, &call.connects),
            // The interpreter this crate embeds for a PYTHON guest, which holds that
            // engine's own seams and bounds ([`super::guest_python`]).
            Guest::Python(process) => process.answer(&call.request, &call.connects),
            // The interpreter's own server, answering out of the machine's filesystem.
            Guest::Files(served) => {
                super::guest_file_server::answer(machine, served, &call.request)
            }
            // A guest that did not start answers nothing, however often it is asked:
            // nothing is listening inside this MicroVM.
            Guest::None(outcome) => outcome.clone(),
        };
        let _ = call.reply.send(outcome);
    }
}

/// What a guest PROCESS runs, once its image's own declaration has started it.
enum Guest {
    /// A program the embedded engine compiled and started.
    Engine(Engine),
    /// A PYTHON program, in the RustPython interpreter this crate embeds
    /// ([`super::guest_python`]), which holds the whole of that engine's own seams and
    /// bounds.
    Python(Box<super::guest_python::PythonProcess>),
    /// The interpreter's own HTTP file server, over the machine's filesystem
    /// ([`super::guest_file_server`]).
    Files(::service_common::static_web_servers::ServedModule),
    /// Nothing runs inside this MicroVM: every request is answered with this.
    None(GuestOutcome),
}

impl Guest {
    /// The ports this guest is listening on, which is what the endpoint reads the port it
    /// FRONTS from — empty where nothing is listening inside the MicroVM at all.
    fn ports(&self) -> Vec<i32> {
        match self {
            Guest::Engine(engine) => engine.ports.clone(),
            Guest::Python(process) => process.ports().to_vec(),
            Guest::Files(served) => vec![served.port],
            Guest::None(_) => Vec::new(),
        }
    }
}

/// START what the image's own container specification declares
/// ([`super::guest_program::GuestStart`]): the module it names, in the engine that module's
/// own suffix names, or the interpreter's own server where the command starts that instead.
///
/// Each of the two languages this emulator embeds an engine for is started in that engine, on
/// the same machine and with the same live-process slot, so what a client reads back is the
/// guest's own answer whichever one its image declares.
fn start_guest(
    program: &GuestProgram,
    machine: &Arc<GuestMachine>,
    slot: &GuestSlot,
    seams: &Seams,
) -> Guest {
    match &program.start {
        GuestStart::Module { language, .. } => match language {
            GuestLanguage::JavaScript => match Engine::start(program, seams) {
                Ok(engine) => Guest::Engine(engine),
                Err(outcome) => Guest::None(outcome),
            },
            GuestLanguage::Python => {
                match super::guest_python::PythonProcess::start(
                    program,
                    Arc::clone(machine),
                    slot.clone(),
                ) {
                    Ok(process) => Guest::Python(Box::new(process)),
                    Err(outcome) => Guest::None(outcome),
                }
            }
        },
        GuestStart::FileServer(served) => Guest::Files(served.clone()),
    }
}

/// The guest processes one store runs: what hands a request to the right one, and the
/// ceiling on how many engines this emulator holds at once.
impl ServiceState {
    /// The guest process of the MicroVM `microvm_id` in `region`, STARTING it where the
    /// MicroVM is running one and this store does not hold it yet.
    ///
    /// Starting it is a write the MicroVM's own lifecycle already owed: it reached
    /// `RUNNING`, which is where its guest is executing, so the engine is materialised at
    /// the first request that needs it rather than reported as something it is not. Every
    /// later request reaches that SAME process, which is what lets the guest keep its
    /// memory between them.
    ///
    /// `None` where the MicroVM holds no machine (it has not started, or has
    /// terminated), where its image version holds no program this engine can run, or
    /// where no live-process slot was free — each of which leaves the endpoint to answer
    /// for a MicroVM it reached nothing inside of.
    pub fn guest_process(
        &mut self,
        region: &str,
        microvm_id: &str,
        now: Instant,
    ) -> Option<Arc<GuestProcess>> {
        let key = (region.to_string(), microvm_id.to_string());
        if let Some(live) = self
            .guest_processes
            .get_mut(&key)
            .filter(|live| live.process.is_running())
        {
            live.last_used = now;
            return Some(Arc::clone(&live.process));
        }

        let vm = self.microvm(region, microvm_id)?;
        let machine = vm.guest_machine.clone()?;
        let (image_arn, image_version) = (vm.image_arn.clone(), vm.image_version.clone());
        let program = self
            .image(region, &image_arn)?
            .versions
            .get(&image_version)?
            .guest
            .clone()?;

        // A process whose own thread has ended holds nothing, and the MicroVM whose
        // request has been idle longest is the one this store lets go of at the ceiling:
        // letting go of it is what frees its engine, and with it the slot the next process
        // takes. A request that still finds no slot is answered rather than held, so the
        // engines this process holds are bounded whatever a client runs.
        self.guest_processes
            .retain(|_, live| live.process.is_running());
        while self.guest_processes.len() >= MAX_LIVE_GUESTS {
            let idlest = self
                .guest_processes
                .iter()
                .min_by_key(|(_, live)| live.last_used)
                .map(|(key, _)| key.clone());
            match idlest {
                Some(key) => {
                    ::tracing::debug!(
                        microvm_id = %key.1,
                        "the guest process idle longest made room at this emulator's ceiling"
                    );
                    self.guest_processes.remove(&key);
                }
                None => break,
            }
        }

        let process = Arc::new(GuestProcess::boot(program, machine)?);
        self.guest_processes.insert(
            key,
            super::LiveGuest {
                process: Arc::clone(&process),
                last_used: now,
            },
        );
        Some(process)
    }
}

/// Run `program` on a machine of its own and answer ONE request with it — the one-shot
/// form the rules of this module are read over in its own tests.
#[cfg(test)]
fn run(program: &GuestProgram, request: &GuestRequest, connects: &ConnectSeam) -> GuestOutcome {
    let machine = Arc::new(GuestMachine::boot(
        "11111111-2222-4333-8444-555555555555".to_string(),
        "mvm-0123456789abcdef".to_string(),
        Some(program),
        "ARM_64",
        ::std::collections::BTreeMap::new(),
        Instant::now(),
        Instant::now(),
    ));
    let seams = Seams::new(machine);
    match Engine::start(program, &seams) {
        Ok(engine) => engine.answer(&seams, request, connects),
        Err(outcome) => outcome,
    }
}

/// The engine ONE guest process runs in: the runtime, the context its program was started
/// in, and the two driver functions the host calls into it.
///
/// Held on the process's own thread and never shared, which is what lets the guest keep
/// its memory between requests.
struct Engine {
    /// The runtime's request sink, held OUTSIDE the engine's global object so a guest that
    /// rebinds its globals cannot replace what hands it a request.
    dispatch: ::rquickjs::Persistent<Function<'static>>,
    /// The guest clock's own driver, held outside the global object for the same reason.
    tick: ::rquickjs::Persistent<Function<'static>>,
    /// The ports the guest's servers bound when its program started, in the order it bound
    /// them; `0` for a server it registered without naming a port. Empty means nothing is
    /// listening inside this MicroVM.
    ports: Vec<i32>,
    /// The context the guest's modules were loaded in.
    ///
    /// Declared AFTER the values held into it and BEFORE the runtime: a struct's fields
    /// drop in declaration order, and the engine cannot be torn down while a value of it
    /// is still held.
    context: Context,
    /// The engine itself; dropping it frees its heap.
    runtime: Runtime,
}

/// The engine stops being asked whether a request is over before it is torn down: what
/// QuickJS does to free its own heap is not a guest's work, and no request is in flight.
impl Drop for Engine {
    fn drop(&mut self) {
        self.runtime.set_interrupt_handler(None);
    }
}

impl Engine {
    /// Build the engine and START the guest's program in it.
    ///
    /// The error is what EVERY request to this process is answered with: an engine that
    /// did not start, or a program that raised or registered no server, is a MicroVM
    /// nothing is listening inside of.
    fn start(program: &GuestProgram, seams: &Seams) -> Result<Self, GuestOutcome> {
        let runtime = Runtime::new()
            .map_err(|error| GuestOutcome::Unavailable(format!("the engine did not start: {error}")))?;
        runtime.set_memory_limit(RUN_MEMORY_BYTES);
        runtime.set_max_stack_size(RUN_STACK_BYTES);
        let watched = Arc::clone(&seams.bounds);
        // The only thing that can stop a guest with no exit: a timeout cannot preempt a
        // loop running on this thread, but the engine asks this handler as it works.
        runtime.set_interrupt_handler(Some(Box::new(move || watched.spent())));
        let context = Context::full(&runtime)
            .map_err(|error| GuestOutcome::Unavailable(format!("the engine did not start: {error}")))?;

        // The guest's own startup is held to the same budget one request is, so a program
        // that never returns does not hold the process's thread either.
        seams.bounds.begin();
        let started = context.with(|ctx| hand_over(ctx, program, seams));
        // A program whose top-level code is still settling promises is driven through
        // them, so a guest that registers its server from one is listening by the time a
        // request arrives. Its TIMERS are left to the requests, which is what drives them.
        execute_jobs(&runtime, &seams.bounds);
        context.with(|ctx| drop(ctx.catch()));
        for line in seams.logs() {
            ::tracing::debug!(line = ?line, "a MicroVM guest wrote a log line");
        }
        // A program that did not start leaves nothing to hand a request to, so the engine
        // it would have run in is torn down here rather than kept: the handler goes with
        // it, because nothing is in flight to stop.
        let started = match started {
            Ok(started) => started,
            Err(outcome) => {
                runtime.set_interrupt_handler(None);
                return Err(outcome);
            }
        };
        if let Some(raised) = started.raised {
            drop(started.dispatch);
            drop(started.tick);
            runtime.set_interrupt_handler(None);
            return Err(GuestOutcome::NoResponse(format!(
                "the guest raised before it answered: {raised}"
            )));
        }
        if started.ports.is_empty() {
            drop(started.dispatch);
            drop(started.tick);
            runtime.set_interrupt_handler(None);
            return Err(GuestOutcome::NoResponse(NO_LISTENER.to_string()));
        }
        Ok(Self {
            dispatch: started.dispatch,
            tick: started.tick,
            ports: started.ports,
            context,
            runtime,
        })
    }

    /// Hand ONE request to the guest already running in this engine and report what it
    /// answered.
    fn answer(
        &self,
        seams: &Seams,
        request: &GuestRequest,
        connects: &ConnectSeam,
    ) -> GuestOutcome {
        // Every bound is spent per request, and the seams point at THIS request's client.
        seams.begin(connects.clone());
        let headers = ::serde_json::to_string(&request.headers).unwrap_or_else(|_| "[]".to_string());
        let dispatched = self.context.with(|ctx| {
            let dispatch = match self.dispatch.clone().restore(&ctx) {
                Ok(dispatch) => dispatch,
                Err(error) => return Err(error.to_string()),
            };
            match dispatch
                .call::<_, bool>((
                    request.port,
                    request.method.clone(),
                    request.url.clone(),
                    headers,
                    request.body.clone(),
                ))
                .catch(&ctx)
            {
                Ok(served) => Ok(served),
                Err(caught) => Err(raised_message(&caught)),
            }
        });
        // A request addressing a port the guest bound no server on reached nothing inside
        // the MicroVM, whatever else the guest is listening on.
        if dispatched == Ok(false) {
            return GuestOutcome::NoResponse(format!(
                "nothing inside the MicroVM is listening on port {}",
                request.port
            ));
        }
        let raised = dispatched.err();
        // A guest that has not answered yet is still working: its promises are settled by
        // the engine's jobs and its callbacks by its own timers, and nothing but this
        // drives them.
        if seams.answer().is_none() {
            drain(&self.runtime, &self.context, &self.tick, &seams.bounds, &seams.answered);
        }
        // The request is over; nothing the engine does between requests is the guest's
        // work.
        self.context.with(|ctx| drop(ctx.catch()));

        for line in seams.logs() {
            // The guest's own output, rendered in its escaped form so a line it wrote
            // cannot forge a log line of this emulator's.
            ::tracing::debug!(line = ?line, "a MicroVM guest wrote a log line");
        }

        // A breached bound is the request's answer, whatever the guest then said: the
        // latch is host state the guest cannot reach.
        if let Some(breach) = seams.bounds.breach() {
            return GuestOutcome::NoResponse(breach);
        }
        match seams.answer() {
            Some(answer) => GuestOutcome::Answered(answer),
            None => GuestOutcome::NoResponse(match raised {
                Some(raised) => format!("the guest raised before it answered: {raised}"),
                None => NO_RESPONSE.to_string(),
            }),
        }
    }
}

/// What starting the guest's program produced.
struct Started {
    /// The runtime's request sink.
    dispatch: ::rquickjs::Persistent<Function<'static>>,
    /// The guest clock's own driver.
    tick: ::rquickjs::Persistent<Function<'static>>,
    /// The ports the guest's servers bound; see [`Engine::ports`].
    ports: Vec<i32>,
    /// Whether the guest's code raised while starting, and what it said.
    raised: Option<String>,
}

/// Settle the engine's pending jobs, under the bounds of the work in flight.
fn execute_jobs(runtime: &Runtime, bounds: &Arc<Bounds>) -> usize {
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
    jobs
}

/// The per-request seams the guest's runtime is built over, and the budget one request is
/// held to: the host state one call writes into and the next call resets.
struct Seams {
    /// The response the guest answered THIS request with.
    answered: Arc<Mutex<Option<GuestAnswer>>>,
    /// The log lines it wrote answering it.
    collected: Arc<Mutex<Collected>>,
    /// The seam this request's outbound connections are decided over, and `None` between
    /// requests — where a connection the guest opens is one nothing carried.
    connects: Arc<Mutex<Option<ConnectSeam>>>,
    /// How many connections THIS request has opened.
    opened: Arc<AtomicU64>,
    /// The budget this request is held to.
    bounds: Arc<Bounds>,
    /// The machine the guest's `fs`, `process` and `os` answer from.
    machine: Arc<GuestMachine>,
}

impl Seams {
    /// The seams of one guest process, before its first request.
    fn new(machine: Arc<GuestMachine>) -> Self {
        Self {
            answered: Arc::new(Mutex::new(None)),
            collected: Arc::new(Mutex::new(Collected::default())),
            connects: Arc::new(Mutex::new(None)),
            opened: Arc::new(AtomicU64::new(0)),
            bounds: Arc::new(Bounds::new()),
            machine,
        }
    }

    /// Start one request: nothing of the request before it is left in the seams, and the
    /// budget is the new request's own.
    fn begin(&self, connects: ConnectSeam) {
        *self.answered.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.collected.lock().unwrap_or_else(PoisonError::into_inner) = Collected::default();
        *self.connects.lock().unwrap_or_else(PoisonError::into_inner) = Some(connects);
        self.opened.store(0, Ordering::Relaxed);
        self.bounds.begin();
    }

    /// What the guest answered the request in flight with.
    fn answer(&self) -> Option<GuestAnswer> {
        self.answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The lines the guest wrote answering it.
    fn logs(&self) -> Vec<String> {
        self.collected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .logs
            .clone()
    }
}

/// What a guest that registered no server is reported as.
const NO_LISTENER: &str = "the guest registered no HTTP server, so nothing is listening";

/// What a guest that registered a server and never answered is reported as.
const NO_RESPONSE: &str = "the guest did not answer the request";

/// Compile the guest's program, START it, and report what that produced, inside the
/// engine context the process keeps.
fn hand_over(
    ctx: Ctx<'_>,
    program: &GuestProgram,
    seams: &Seams,
) -> Result<Started, GuestOutcome> {
    install_host_seams(&ctx, seams)
        .map_err(|reason| GuestOutcome::Unavailable(format!("the engine did not start: {reason}")))?;
    // Held BEFORE any of the guest's code is compiled, so the runtime its requests are
    // driven through is this module's own however the guest rebinds the global object.
    let runtime: Object<'_> = ctx
        .eval::<Object<'_>, _>(RUNTIME_SOURCE)
        .catch(&ctx)
        .map_err(|caught| {
            GuestOutcome::Unavailable(format!("the guest runtime could not be installed: {caught}"))
        })?;

    let modules = ::serde_json::to_string(&program.modules).unwrap_or_else(|_| "{}".to_string());
    let modules = ctx.json_parse(modules).map_err(|error| {
        GuestOutcome::Unavailable(format!("the guest's modules could not be read: {error}"))
    })?;
    let start: Function<'_> = runtime.get("start").map_err(|error| {
        GuestOutcome::Unavailable(format!("the guest runtime has no entry point: {error}"))
    })?;
    let dispatch: Function<'_> = runtime.get("dispatch").map_err(|error| {
        GuestOutcome::Unavailable(format!("the guest runtime has no request sink: {error}"))
    })?;
    let tick: Function<'_> = runtime.get("tick").map_err(|error| {
        GuestOutcome::Unavailable(format!("the guest runtime has no clock: {error}"))
    })?;
    let tick = ::rquickjs::Persistent::save(&ctx, tick);
    let dispatch = ::rquickjs::Persistent::save(&ctx, dispatch);

    // From here on what runs is the GUEST's: its own modules, started the way its image's
    // command starts them, and the ports they bound are what it is listening on.
    match start
        .call::<_, String>((modules, program.entry().to_string()))
        .catch(&ctx)
    {
        Ok(bound) => Ok(Started {
            dispatch,
            tick,
            ports: ::serde_json::from_str::<Vec<i32>>(&bound).unwrap_or_default(),
            raised: None,
        }),
        Err(caught) => Ok(Started {
            dispatch,
            tick,
            ports: Vec::new(),
            raised: Some(raised_message(&caught)),
        }),
    }
}

/// Drive the guest until it answers, until it has nothing left to do, or until a bound
/// ends the run: the engine's jobs first, then the earliest timer callback the guest's
/// own clock has due.
fn drain(
    runtime: &Runtime,
    context: &Context,
    tick: &::rquickjs::Persistent<Function<'static>>,
    bounds: &Arc<Bounds>,
    answered: &Arc<Mutex<Option<GuestAnswer>>>,
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
            tick.clone()
                .restore(&ctx)
                .and_then(|tick| tick.call::<_, bool>(()))
                .unwrap_or(false)
        });
        if !more {
            return;
        }
    }
}

/// The global the response sink is handed to the engine as.
const HOST_RESPOND: &str = "__microvmHostRespond";

/// The global the outbound-connection seam is handed to the engine as.
const HOST_CONNECT: &str = "__microvmHostConnect";

/// The global the log writer is handed to the engine as.
const HOST_LOG: &str = "__microvmHostLog";

/// The global the MACHINE the guest runs on is handed to the engine as: the identity, the
/// environment and the command line its MicroVM published to it, as JSON.
const HOST_MACHINE: &str = "__microvmHostMachine";

/// The global the machine's clocks are read through.
const HOST_CLOCKS: &str = "__microvmHostClocks";

/// The global the machine's filesystem is read through.
const HOST_FS_READ: &str = "__microvmHostFsRead";

/// The global it is written through.
const HOST_FS_WRITE: &str = "__microvmHostFsWrite";

/// The global a file is removed through.
const HOST_FS_REMOVE: &str = "__microvmHostFsRemove";

/// The global a directory is made through.
const HOST_FS_MKDIR: &str = "__microvmHostFsMkdir";

/// The global a path is reported through.
const HOST_FS_STAT: &str = "__microvmHostFsStat";

/// Install the host functions the guest's runtime is built over.
///
/// They are the only host functions reachable from inside the engine, and
/// [`RUNTIME_SOURCE`] deletes their global bindings as soon as it holds them, so after it
/// has run they are reachable from nothing but its own closures. Each of the machine's
/// seams answers from [`GuestMachine`] — the MicroVM's own identity and tree — and none of
/// them opens, resolves or reads anything of the host.
fn install_host_seams(ctx: &Ctx<'_>, seams: &Seams) -> Result<(), String> {
    let latch = Arc::clone(&seams.answered);
    let respond = Func::from(move |status: i32, headers: String, body: String| {
        let mut held = latch.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_some() {
            // A guest that answers twice has answered once: the first response is the
            // one the client reads, exactly as a server that has already flushed its
            // headers cannot take them back.
            return;
        }
        let mut body = body.into_bytes();
        body.truncate(MAX_RESPONSE_BYTES);
        *held = Some(GuestAnswer {
            status: u16::try_from(status).unwrap_or(200),
            headers: ::serde_json::from_str::<Vec<(String, String)>>(&headers).unwrap_or_default(),
            body,
        });
    });

    let seam = Arc::clone(&seams.connects);
    let opened = Arc::clone(&seams.opened);
    let connect = Func::from(move |host: String, port: i32| -> String {
        let taken = opened.fetch_add(1, Ordering::Relaxed);
        if taken >= MAX_CONNECTS {
            return verdict_json(&ConnectVerdict::unreached());
        }
        // The request in flight is what decides a connection; a guest that opens one
        // between requests (from a timer of its own) has nothing to carry it.
        let Some(seam) = seam
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return verdict_json(&ConnectVerdict::unreached());
        };
        let (reply, answer) = ::std::sync::mpsc::channel();
        if seam
            .send(ConnectAsk {
                host,
                port,
                reply,
            })
            .is_err()
        {
            return verdict_json(&ConnectVerdict::unreached());
        }
        let verdict = answer
            .recv_timeout(CONNECT_DECISION_CEILING)
            .unwrap_or_else(|_| ConnectVerdict::unreached());
        verdict_json(&verdict)
    });

    let logged = Arc::clone(&seams.collected);
    let log = Func::from(move |message: String| {
        logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .log(&message);
    });

    let machine = Arc::clone(&seams.machine);
    let clocks = Func::from(move || -> String {
        let now = Instant::now();
        ::serde_json::json!({
            "processUptime": machine.process_uptime(now),
            "machineUptime": machine.machine_uptime(now),
        })
        .to_string()
    });

    let machine = Arc::clone(&seams.machine);
    let read = Func::from(move |path: String| -> Option<String> { machine.read(&path) });

    let machine = Arc::clone(&seams.machine);
    let write = Func::from(move |path: String, contents: String, append: bool| -> bool {
        machine.write(&path, &contents, append)
    });

    let machine = Arc::clone(&seams.machine);
    let remove = Func::from(move |path: String| -> bool { machine.remove(&path) });

    let machine = Arc::clone(&seams.machine);
    let mkdir = Func::from(move |path: String| -> bool { machine.make_directories(&path) });

    let machine = Arc::clone(&seams.machine);
    let stat = Func::from(move |path: String| -> String {
        let held = machine.stat(&path);
        ::serde_json::json!({
            "exists": held.exists,
            "file": held.file,
            "directory": held.directory,
            "size": held.size,
        })
        .to_string()
    });

    let globals = ctx.globals();
    globals
        .set(HOST_RESPOND, respond)
        .and_then(|()| globals.set(HOST_CONNECT, connect))
        .and_then(|()| globals.set(HOST_LOG, log))
        .and_then(|()| globals.set(HOST_MACHINE, machine_json(&seams.machine)))
        .and_then(|()| globals.set(HOST_CLOCKS, clocks))
        .and_then(|()| globals.set(HOST_FS_READ, read))
        .and_then(|()| globals.set(HOST_FS_WRITE, write))
        .and_then(|()| globals.set(HOST_FS_REMOVE, remove))
        .and_then(|()| globals.set(HOST_FS_MKDIR, mkdir))
        .and_then(|()| globals.set(HOST_FS_STAT, stat))
        .map_err(|error| error.to_string())
}

/// The machine, as the guest's `process` and `os` read it: what its MicroVM published to
/// it, and nothing of the process this emulator runs in.
fn machine_json(machine: &Arc<GuestMachine>) -> String {
    ::serde_json::json!({
        "pid": super::guest_machine::GUEST_PID,
        "bootId": machine.boot_id,
        "hostname": machine.hostname,
        "env": machine.env,
        "argv": machine.argv(),
        "execPath": machine.interpreter_path(),
        "cwd": super::guest_machine::GUEST_WORKING_DIRECTORY,
        "platform": machine.platform(),
        "arch": machine.arch(),
        "kernelRelease": machine.kernel_release(),
        "nodeVersion": machine.node_version(),
    })
    .to_string()
}

/// One connection decision, as the guest's `net` reads it back.
fn verdict_json(verdict: &ConnectVerdict) -> String {
    ::serde_json::json!({
        "connected": verdict.connected,
        "localAddress": verdict.local_address,
        "localPort": verdict.local_port,
    })
    .to_string()
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

/// The log lines one run wrote, under this module's ceilings.
#[derive(Debug, Default)]
struct Collected {
    logs: Vec<String>,
}

impl Collected {
    /// Record one line, under the count and length ceilings.
    fn log(&mut self, message: &str) {
        if let Some(line) = capped_log_line(self.logs.len(), message) {
            self.logs.push(line);
        }
    }
}

/// One log line a guest wrote, under this module's ceilings — the count of lines already
/// held decides whether there is room for it at all, and a line past the length ceiling is
/// cut on a character boundary so what is kept is still a string.
///
/// Shared by both engines ([`super::guest_python`]), so what a guest's log costs this process
/// is the same whichever language its program is written in.
pub(super) fn capped_log_line(held: usize, message: &str) -> Option<String> {
    if held >= MAX_LOG_ENTRIES {
        return None;
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
    Some(line)
}

/// The budget one REQUEST is held to, as the engine's interrupt handler spends it, and
/// the latch that says which bound ended it.
///
/// One guest process answers request after request in the same engine, so the budget is
/// reset at the start of each ([`Self::begin`]) rather than built per engine: what a
/// guest spent answering one client is never charged to the next.
#[derive(Debug)]
struct Bounds {
    /// The instant this budget measures its clock from, fixed for the life of the
    /// process so the deadline below can be read and written without a lock.
    origin: Instant,
    /// When the request in flight must be over, in milliseconds since [`Self::origin`].
    deadline: AtomicU64,
    /// How many engine checks one request starts with, which a breach reports.
    budget: u64,
    /// How many engine checks are left.
    checks: AtomicU64,
    /// Set once the clock ran out.
    timed_out: AtomicBool,
    /// Set once the checks ran out.
    steps_spent: AtomicBool,
}

impl Bounds {
    /// A budget of the checks and the clock one request is held to.
    fn new() -> Self {
        Self::with_budget(RUN_ENGINE_CHECKS)
    }

    /// A budget of `budget` checks starting now — the one constructor, so a budget small
    /// enough to spend in a test is spent through exactly the path a request's is.
    fn with_budget(budget: u64) -> Self {
        let bounds = Self {
            origin: Instant::now(),
            deadline: AtomicU64::new(0),
            budget,
            checks: AtomicU64::new(budget),
            timed_out: AtomicBool::new(false),
            steps_spent: AtomicBool::new(false),
        };
        bounds.begin();
        bounds
    }

    /// Start one request's budget: its whole clock and its whole step budget, with no
    /// breach of the request before it left latched.
    fn begin(&self) {
        let wall_clock = u64::try_from(RUN_WALL_CLOCK.as_millis()).unwrap_or(u64::MAX);
        self.deadline.store(
            self.elapsed().saturating_add(wall_clock),
            Ordering::Relaxed,
        );
        self.checks.store(self.budget, Ordering::Relaxed);
        self.timed_out.store(false, Ordering::Relaxed);
        self.steps_spent.store(false, Ordering::Relaxed);
    }

    /// How long this budget's own clock has been running, in milliseconds.
    fn elapsed(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Whether the request is over, asked by the engine as it works: the latch first, then
    /// the clock, then the step budget. Each answer latches, so a breach survives a guest
    /// that catches the exception the engine raises in it.
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

    /// Whether the request's clock has run out, latching it when it has. Read by the host
    /// where the engine is not running code of its own, so a request is stopped between
    /// jobs as well as inside one.
    fn stop_if_due(&self) -> bool {
        if self.steps_spent.load(Ordering::Relaxed) {
            return true;
        }
        if self.elapsed() >= self.deadline.load(Ordering::Relaxed) {
            self.timed_out.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// What the breached bound is reported as, or `None` when none was breached.
    fn breach(&self) -> Option<String> {
        if self.timed_out.load(Ordering::Relaxed) {
            return Some(format!(
                "the guest was stopped after {} seconds without answering",
                RUN_WALL_CLOCK.as_secs()
            ));
        }
        if self.steps_spent.load(Ordering::Relaxed) {
            return Some(format!(
                "the guest was stopped after {} engine steps without answering",
                self.budget
            ));
        }
        None
    }
}

/// The runtime a MicroVM guest runs inside: the modules it can require, the timers it can
/// set, the server it registers and the sockets it opens.
///
/// It is an expression evaluating to the driver the HOST calls, and it captures the three
/// host functions and the engine builtins it needs before any of the guest's code is
/// compiled, so a guest that rebinds `JSON`, `String` or the global object cannot change
/// how its own response is rendered or reach the host functions again.
///
/// Every module here is this emulator's own: `net` opens no socket (where a connection
/// goes is the host's decision), `http` serves the one proxied request, and there is no
/// filesystem, process, environment or thread module at all — a guest requiring one gets
/// the refusal a missing module gets.
const RUNTIME_SOURCE: &str = r##"
(function () {
    var hostRespond = globalThis.__microvmHostRespond;
    var hostConnect = globalThis.__microvmHostConnect;
    var hostLog = globalThis.__microvmHostLog;
    var hostMachine = globalThis.__microvmHostMachine;
    var hostClocks = globalThis.__microvmHostClocks;
    var hostFsRead = globalThis.__microvmHostFsRead;
    var hostFsWrite = globalThis.__microvmHostFsWrite;
    var hostFsRemove = globalThis.__microvmHostFsRemove;
    var hostFsMkdir = globalThis.__microvmHostFsMkdir;
    var hostFsStat = globalThis.__microvmHostFsStat;
    delete globalThis.__microvmHostRespond;
    delete globalThis.__microvmHostConnect;
    delete globalThis.__microvmHostLog;
    delete globalThis.__microvmHostMachine;
    delete globalThis.__microvmHostClocks;
    delete globalThis.__microvmHostFsRead;
    delete globalThis.__microvmHostFsWrite;
    delete globalThis.__microvmHostFsRemove;
    delete globalThis.__microvmHostFsMkdir;
    delete globalThis.__microvmHostFsStat;
    var stringify = JSON.stringify;
    var parse = JSON.parse;
    var toText = String;

    /* ---- the guest's own clock: due instants, never a sleep ---- */
    var clock = 0;
    var timers = [];
    var order = 0;
    function schedule(fn, delay, args, every) {
        if (typeof fn !== "function") { return { live: false }; }
        var due = Number(delay);
        if (!isFinite(due) || due < 0) { due = 0; }
        var timer = { at: clock + due, seq: order++, fn: fn, args: args || [],
                      live: true, every: every ? due : null };
        timers.push(timer);
        return timer;
    }
    function clearTimer(timer) { if (timer && typeof timer === "object") { timer.live = false; } }
    function tick() {
        var next = null;
        for (var i = 0; i < timers.length; i++) {
            var timer = timers[i];
            if (!timer.live) { continue; }
            if (next === null || timer.at < next.at || (timer.at === next.at && timer.seq < next.seq)) {
                next = timer;
            }
        }
        if (next === null) { return false; }
        if (next.at > clock) { clock = next.at; }
        if (next.every === null) { next.live = false; } else { next.at = clock + next.every; }
        next.fn.apply(null, next.args);
        return true;
    }

    /* ---- the console the guest writes through ---- */
    function rendered(value) {
        if (typeof value === "string") { return value; }
        if (value instanceof Error) { return toText(value); }
        try {
            var text = stringify(value);
            return text === undefined ? toText(value) : text;
        } catch (error) { return toText(value); }
    }
    function write() {
        hostLog(Array.prototype.map.call(arguments, rendered).join(" "));
    }

    /* ---- events ---- */
    function Emitter() { this._listeners = {}; }
    Emitter.prototype.on = function (name, fn) {
        var held = this._listeners[name] || (this._listeners[name] = []);
        held.push(fn);
        return this;
    };
    Emitter.prototype.addListener = Emitter.prototype.on;
    Emitter.prototype.once = function (name, fn) {
        var self = this;
        var wrapper = function () { self.removeListener(name, wrapper); fn.apply(self, arguments); };
        return this.on(name, wrapper);
    };
    Emitter.prototype.removeListener = function (name, fn) {
        var held = this._listeners[name];
        if (held) { this._listeners[name] = held.filter(function (one) { return one !== fn; }); }
        return this;
    };
    Emitter.prototype.removeAllListeners = function (name) {
        if (name === undefined) { this._listeners = {}; } else { delete this._listeners[name]; }
        return this;
    };
    Emitter.prototype.setMaxListeners = function () { return this; };
    Emitter.prototype.listenerCount = function (name) {
        return (this._listeners[name] || []).length;
    };
    Emitter.prototype.emit = function (name) {
        var held = (this._listeners[name] || []).slice();
        var args = Array.prototype.slice.call(arguments, 1);
        for (var i = 0; i < held.length; i++) { held[i].apply(this, args); }
        return held.length > 0;
    };

    /* ---- query strings and URLs ---- */
    function parseQuery(text) {
        var query = {};
        if (!text) { return query; }
        var pairs = toText(text).replace(/^\?/, "").split("&");
        for (var i = 0; i < pairs.length; i++) {
            if (pairs[i].length === 0) { continue; }
            var at = pairs[i].indexOf("=");
            var key = at < 0 ? pairs[i] : pairs[i].slice(0, at);
            var value = at < 0 ? "" : pairs[i].slice(at + 1);
            key = decodeURIComponent(key.replace(/\+/g, " "));
            value = decodeURIComponent(value.replace(/\+/g, " "));
            if (query[key] === undefined) { query[key] = value; }
            else if (Array.isArray(query[key])) { query[key].push(value); }
            else { query[key] = [query[key], value]; }
        }
        return query;
    }
    function stringifyQuery(query) {
        var parts = [];
        Object.keys(query || {}).forEach(function (key) {
            var value = query[key];
            var values = Array.isArray(value) ? value : [value];
            values.forEach(function (one) {
                parts.push(encodeURIComponent(key) + "=" + encodeURIComponent(one));
            });
        });
        return parts.join("&");
    }
    function parseUrl(target, withQuery) {
        var text = toText(target === undefined || target === null ? "" : target);
        var hash = "";
        var at = text.indexOf("#");
        if (at >= 0) { hash = text.slice(at); text = text.slice(0, at); }
        var search = "";
        at = text.indexOf("?");
        if (at >= 0) { search = text.slice(at); text = text.slice(0, at); }
        return {
            pathname: text,
            path: text + search,
            search: search.length > 0 ? search : null,
            query: withQuery ? parseQuery(search) : (search.length > 0 ? search.slice(1) : null),
            hash: hash.length > 0 ? hash : null,
            href: text + search + hash
        };
    }
    function SearchParams(init) {
        this._pairs = [];
        var query = typeof init === "string" ? parseQuery(init) : (init || {});
        var self = this;
        Object.keys(query).forEach(function (key) {
            var value = query[key];
            var values = Array.isArray(value) ? value : [value];
            values.forEach(function (one) { self._pairs.push([key, toText(one)]); });
        });
    }
    SearchParams.prototype.get = function (key) {
        for (var i = 0; i < this._pairs.length; i++) {
            if (this._pairs[i][0] === key) { return this._pairs[i][1]; }
        }
        return null;
    };
    SearchParams.prototype.getAll = function (key) {
        return this._pairs.filter(function (pair) { return pair[0] === key; })
                          .map(function (pair) { return pair[1]; });
    };
    SearchParams.prototype.has = function (key) { return this.get(key) !== null; };
    SearchParams.prototype.toString = function () {
        return this._pairs.map(function (pair) {
            return encodeURIComponent(pair[0]) + "=" + encodeURIComponent(pair[1]);
        }).join("&");
    };
    function Url(target, base) {
        var text = toText(target);
        if (base !== undefined && text.indexOf("://") < 0) {
            var root = toText(base).replace(/\/+$/, "");
            text = root + (text.charAt(0) === "/" ? text : "/" + text);
        }
        var scheme = "";
        var at = text.indexOf("://");
        if (at >= 0) { scheme = text.slice(0, at + 1); text = text.slice(at + 3); }
        var authority = "";
        at = text.indexOf("/");
        if (scheme.length > 0) {
            authority = at < 0 ? text : text.slice(0, at);
            text = at < 0 ? "/" : text.slice(at);
        }
        var parsed = parseUrl(text, false);
        this.protocol = scheme;
        this.host = authority;
        this.hostname = authority.split(":")[0];
        this.port = authority.indexOf(":") > 0 ? authority.split(":")[1] : "";
        this.pathname = parsed.pathname;
        this.search = parsed.search === null ? "" : parsed.search;
        this.hash = parsed.hash === null ? "" : parsed.hash;
        this.searchParams = new SearchParams(this.search);
        this.href = scheme + (scheme.length > 0 ? "//" : "") + authority + parsed.pathname +
                    this.search + this.hash;
    }
    Url.prototype.toString = function () { return this.href; };

    /* ---- net: the sockets a guest opens, decided by the MicroVM's connectors ---- */
    var UNANSWERED_CONNECT_MS = 130000;
    function Socket() {
        Emitter.call(this);
        this.connecting = false;
        this.destroyed = false;
        this.remoteAddress = null;
        this.remotePort = null;
        this.localAddress = null;
        this.localPort = null;
        this._timeout = null;
        this._pending = null;
    }
    Socket.prototype = Object.create(Emitter.prototype);
    Socket.prototype.constructor = Socket;
    Socket.prototype.setTimeout = function (ms, fn) {
        var self = this;
        clearTimer(this._timeout);
        if (fn) { this.once("timeout", fn); }
        if (Number(ms) > 0) {
            this._timeout = schedule(function () { self.emit("timeout"); }, ms, [], false);
        }
        return this;
    };
    Socket.prototype.setNoDelay = function () { return this; };
    Socket.prototype.setKeepAlive = function () { return this; };
    Socket.prototype.unref = function () { return this; };
    Socket.prototype.ref = function () { return this; };
    /* The destination is a resource of the emulated network rather than a process on
       this machine, so nothing is delivered either way: what a connection establishes
       is reachability. */
    Socket.prototype.write = function () { return true; };
    Socket.prototype.end = function () { this.destroy(); return this; };
    Socket.prototype.destroy = function () {
        if (this.destroyed) { return this; }
        this.destroyed = true;
        this.connecting = false;
        clearTimer(this._timeout);
        clearTimer(this._pending);
        this.emit("close", false);
        return this;
    };
    function connect() {
        var options = arguments[0];
        var callback = null;
        var index = 1;
        if (typeof options === "number" || typeof options === "string") {
            options = { port: Number(options) };
            if (typeof arguments[1] === "string") { options.host = arguments[1]; index = 2; }
        }
        for (; index < arguments.length; index++) {
            if (typeof arguments[index] === "function") { callback = arguments[index]; }
        }
        var socket = new Socket();
        var host = toText((options && options.host) || "127.0.0.1");
        var port = Number((options && options.port) || 0);
        socket.connecting = true;
        var verdict = parse(hostConnect(host, port));
        if (verdict.connected) {
            socket.remoteAddress = host;
            socket.remotePort = port;
            socket.localAddress = verdict.localAddress;
            socket.localPort = verdict.localPort;
            if (callback) { socket.once("connect", callback); }
            socket._pending = schedule(function () {
                if (socket.destroyed) { return; }
                socket.connecting = false;
                socket.emit("connect");
                socket.emit("ready");
            }, 0, [], false);
            return socket;
        }
        /* Nothing carried the connection, so it never completes: the guest's own
           timeout is what ends it, and where it set none this ceiling does. */
        if (callback) { socket.once("connect", callback); }
        socket._pending = schedule(function () {
            if (socket.destroyed) { return; }
            socket.connecting = false;
            var error = new Error("connect ETIMEDOUT " + host + ":" + port);
            error.code = "ETIMEDOUT";
            error.errno = "ETIMEDOUT";
            error.syscall = "connect";
            error.address = host;
            error.port = port;
            if (socket.listenerCount("error") > 0) { socket.emit("error", error); }
            socket.destroy();
        }, UNANSWERED_CONNECT_MS, [], false);
        return socket;
    }

    /* ---- http: the server the guest registers, and the one request it is handed ---- */
    var servers = [];
    function ServerResponse() {
        Emitter.call(this);
        this.statusCode = 200;
        this.statusMessage = null;
        this.headersSent = false;
        this.finished = false;
        this._headers = [];
        this._chunks = [];
    }
    ServerResponse.prototype = Object.create(Emitter.prototype);
    ServerResponse.prototype.constructor = ServerResponse;
    ServerResponse.prototype.setHeader = function (name, value) {
        var held = this._headers.filter(function (pair) {
            return pair[0].toLowerCase() !== toText(name).toLowerCase();
        });
        held.push([toText(name), toText(value)]);
        this._headers = held;
        return this;
    };
    ServerResponse.prototype.getHeader = function (name) {
        var wanted = toText(name).toLowerCase();
        for (var i = 0; i < this._headers.length; i++) {
            if (this._headers[i][0].toLowerCase() === wanted) { return this._headers[i][1]; }
        }
        return undefined;
    };
    ServerResponse.prototype.removeHeader = function (name) {
        var wanted = toText(name).toLowerCase();
        this._headers = this._headers.filter(function (pair) {
            return pair[0].toLowerCase() !== wanted;
        });
        return this;
    };
    ServerResponse.prototype.writeHead = function (status, reason, headers) {
        this.statusCode = Number(status) || 200;
        var given = headers;
        if (reason && typeof reason === "object") { given = reason; }
        else if (typeof reason === "string") { this.statusMessage = reason; }
        if (given) {
            var self = this;
            Object.keys(given).forEach(function (name) { self.setHeader(name, given[name]); });
        }
        this.headersSent = true;
        return this;
    };
    ServerResponse.prototype.write = function (chunk) {
        if (chunk !== undefined && chunk !== null) { this._chunks.push(toText(chunk)); }
        return true;
    };
    ServerResponse.prototype.end = function (chunk) {
        if (this.finished) { return this; }
        this.write(chunk);
        this.finished = true;
        this.headersSent = true;
        hostRespond(this.statusCode, stringify(this._headers), this._chunks.join(""));
        this.emit("finish");
        return this;
    };
    function IncomingMessage(method, url, headers, body) {
        Emitter.call(this);
        this.method = method;
        this.url = url;
        this.httpVersion = "1.1";
        this.headers = headers;
        this.rawHeaders = [];
        var self = this;
        Object.keys(headers).forEach(function (name) {
            self.rawHeaders.push(name);
            self.rawHeaders.push(headers[name]);
        });
        this.socket = { remoteAddress: null, localAddress: null };
        this._body = body;
    }
    IncomingMessage.prototype = Object.create(Emitter.prototype);
    IncomingMessage.prototype.constructor = IncomingMessage;
    IncomingMessage.prototype.setEncoding = function () { return this; };
    IncomingMessage.prototype.resume = function () { return this; };
    IncomingMessage.prototype.pause = function () { return this; };

    function Server(handler) {
        Emitter.call(this);
        this.listening = false;
        this._port = null;
        if (typeof handler === "function") { this.on("request", handler); }
        servers.push(this);
    }
    Server.prototype = Object.create(Emitter.prototype);
    Server.prototype.constructor = Server;
    Server.prototype.listen = function () {
        var self = this;
        var callback = null;
        for (var i = 0; i < arguments.length; i++) {
            if (typeof arguments[i] === "function") { callback = arguments[i]; }
            else if (typeof arguments[i] === "number") { this._port = arguments[i]; }
            else if (typeof arguments[i] === "object" && arguments[i] &&
                     typeof arguments[i].port === "number") { this._port = arguments[i].port; }
        }
        this.listening = true;
        schedule(function () { self.emit("listening"); if (callback) { callback(); } }, 0, [], false);
        return this;
    };
    Server.prototype.address = function () {
        return this.listening ? { address: "0.0.0.0", family: "IPv4", port: this._port } : null;
    };
    Server.prototype.close = function (callback) {
        this.listening = false;
        if (callback) { schedule(callback, 0, [], false); }
        return this;
    };
    Server.prototype.setTimeout = function () { return this; };
    Server.prototype.unref = function () { return this; };
    Server.prototype.ref = function () { return this; };

    /* ---- the machine this guest runs on: what its MicroVM published to it, its
       filesystem, and the process it is ---- */
    var machine = {};
    try { machine = parse(hostMachine || "{}"); } catch (error) { machine = {}; }
    function clocks() {
        try { return parse(hostClocks()); } catch (error) { return {}; }
    }

    function fsFail(code, call, path, reason) {
        var error = new Error(code + ": " + reason + ", " + call + " '" + toText(path) + "'");
        error.code = code;
        error.syscall = call;
        error.path = toText(path);
        return error;
    }
    function heldAt(path) {
        try { return parse(hostFsStat(toText(path))); } catch (error) { return {}; }
    }
    function Stats(held) {
        this.size = Number(held.size) || 0;
        this._file = !!held.file;
        this._directory = !!held.directory;
    }
    Stats.prototype.isFile = function () { return this._file; };
    Stats.prototype.isDirectory = function () { return this._directory; };
    Stats.prototype.isSymbolicLink = function () { return false; };
    function readFileSync(path) {
        var text = hostFsRead(toText(path));
        if (text === null || text === undefined) {
            throw fsFail("ENOENT", "open", path, "no such file or directory");
        }
        return text;
    }
    function writeFileSync(path, data) {
        var text = data === undefined || data === null ? "" : toText(data);
        if (!hostFsWrite(toText(path), text, false)) {
            throw fsFail("EACCES", "open", path, "permission denied");
        }
    }
    function appendFileSync(path, data) {
        var text = data === undefined || data === null ? "" : toText(data);
        if (!hostFsWrite(toText(path), text, true)) {
            throw fsFail("EACCES", "open", path, "permission denied");
        }
    }
    function existsSync(path) { return !!heldAt(path).exists; }
    function statSync(path) {
        var held = heldAt(path);
        if (!held.exists) { throw fsFail("ENOENT", "stat", path, "no such file or directory"); }
        return new Stats(held);
    }
    function mkdirSync(path) {
        if (!hostFsMkdir(toText(path))) {
            throw fsFail("EACCES", "mkdir", path, "permission denied");
        }
    }
    function unlinkSync(path) {
        if (!hostFsRemove(toText(path))) {
            throw fsFail("ENOENT", "unlink", path, "no such file or directory");
        }
    }
    function calledBack(sync) {
        return function () {
            var args = Array.prototype.slice.call(arguments);
            var done = args.length > 0 && typeof args[args.length - 1] === "function"
                ? args.pop() : null;
            var value = null;
            var failure = null;
            try { value = sync.apply(null, args); } catch (error) { failure = error; }
            if (done) { schedule(function () { done(failure, value); }, 0, [], false); }
        };
    }
    function promised(sync) {
        return function () {
            var args = Array.prototype.slice.call(arguments);
            return new Promise(function (resolve, reject) {
                try { resolve(sync.apply(null, args)); } catch (error) { reject(error); }
            });
        };
    }
    var fs = {
        readFileSync: readFileSync, writeFileSync: writeFileSync,
        appendFileSync: appendFileSync, existsSync: existsSync,
        statSync: statSync, lstatSync: statSync, mkdirSync: mkdirSync,
        unlinkSync: unlinkSync, rmSync: unlinkSync,
        readFile: calledBack(readFileSync), writeFile: calledBack(writeFileSync),
        appendFile: calledBack(appendFileSync), mkdir: calledBack(mkdirSync),
        unlink: calledBack(unlinkSync), stat: calledBack(statSync),
        promises: {
            readFile: promised(readFileSync), writeFile: promised(writeFileSync),
            appendFile: promised(appendFileSync), mkdir: promised(mkdirSync),
            unlink: promised(unlinkSync), stat: promised(statSync)
        }
    };

    function normalizePath(input) {
        var text = toText(input);
        var absolute = text.charAt(0) === "/";
        var parts = [];
        text.split("/").forEach(function (segment) {
            if (segment === "" || segment === ".") { return; }
            if (segment === "..") {
                if (parts.length > 0 && parts[parts.length - 1] !== "..") { parts.pop(); }
                else if (!absolute) { parts.push(".."); }
                return;
            }
            parts.push(segment);
        });
        var joined = parts.join("/");
        if (absolute) { return "/" + joined; }
        return joined === "" ? "." : joined;
    }
    var pathModule = {
        sep: "/",
        delimiter: ":",
        normalize: normalizePath,
        isAbsolute: function (path) { return toText(path).charAt(0) === "/"; },
        join: function () {
            var given = Array.prototype.filter.call(arguments, function (one) {
                return one !== undefined && one !== null && toText(one) !== "";
            });
            return given.length === 0 ? "." : normalizePath(given.map(toText).join("/"));
        },
        resolve: function () {
            var resolved = "";
            for (var i = arguments.length - 1; i >= 0; i--) {
                var one = arguments[i];
                if (one === undefined || one === null || toText(one) === "") { continue; }
                one = toText(one);
                resolved = resolved === "" ? one : one + "/" + resolved;
                if (one.charAt(0) === "/") { return normalizePath(resolved); }
            }
            return normalizePath((machine.cwd || "/") + "/" + resolved);
        },
        dirname: function (path) {
            var text = normalizePath(path);
            var cut = text.lastIndexOf("/");
            if (cut < 0) { return "."; }
            return cut === 0 ? "/" : text.slice(0, cut);
        },
        basename: function (path, ext) {
            var text = normalizePath(path);
            var name = text.slice(text.lastIndexOf("/") + 1);
            if (ext && name.length > ext.length && name.slice(-ext.length) === ext) {
                return name.slice(0, name.length - ext.length);
            }
            return name;
        },
        extname: function (path) {
            var name = toText(path).slice(toText(path).lastIndexOf("/") + 1);
            var cut = name.lastIndexOf(".");
            return cut <= 0 ? "" : name.slice(cut);
        }
    };

    var processObject = new Emitter();
    processObject.pid = Number(machine.pid) || 1;
    processObject.ppid = 0;
    processObject.platform = machine.platform || "linux";
    processObject.arch = machine.arch || "x64";
    processObject.env = machine.env || {};
    processObject.argv = machine.argv || [];
    processObject.argv0 = "node";
    processObject.execPath = machine.execPath || "";
    processObject.version = machine.nodeVersion || "";
    processObject.versions = { node: (machine.nodeVersion || "").replace(/^v/, "") };
    processObject.release = { name: "node" };
    processObject.exitCode = undefined;
    processObject.cwd = function () { return machine.cwd || "/"; };
    processObject.uptime = function () { return Number(clocks().processUptime) || 0; };
    processObject.hrtime = function (previous) {
        var now = (Number(clocks().processUptime) || 0);
        var seconds = Math.floor(now);
        var nanoseconds = Math.round((now - seconds) * 1e9);
        if (previous) {
            var before = Number(previous[0]) * 1e9 + Number(previous[1]);
            var elapsed = Math.max(0, seconds * 1e9 + nanoseconds - before);
            return [Math.floor(elapsed / 1e9), elapsed % 1e9];
        }
        return [seconds, nanoseconds];
    };
    processObject.nextTick = function (fn) {
        schedule(fn, 0, Array.prototype.slice.call(arguments, 1), false);
    };
    /* A guest that asks to exit stops serving nothing of its own accord: the MicroVM's
       lifecycle is the control plane's, so the code it asked to exit with is kept the way
       `process.exitCode` keeps one. */
    processObject.exit = function (code) {
        processObject.exitCode = code === undefined ? 0 : Number(code);
    };
    processObject.stdout = { write: function (chunk) { hostLog(toText(chunk)); return true; },
                             isTTY: false, columns: 80 };
    processObject.stderr = { write: function (chunk) { hostLog(toText(chunk)); return true; },
                             isTTY: false, columns: 80 };
    processObject.emitWarning = function (warning) { hostLog(rendered(warning)); };

    /* ---- the modules a guest can require ---- */
    var core = {
        events: Emitter,
        http: { createServer: function (handler) { return new Server(handler); },
                Server: Server, ServerResponse: ServerResponse,
                IncomingMessage: IncomingMessage, STATUS_CODES: {} },
        net: { connect: connect, createConnection: connect, Socket: Socket },
        url: { parse: parseUrl, URL: Url, URLSearchParams: SearchParams,
               format: function (parts) { return toText(parts && parts.href || ""); } },
        querystring: { parse: parseQuery, stringify: stringifyQuery,
                       escape: encodeURIComponent, unescape: decodeURIComponent },
        fs: fs,
        path: pathModule,
        process: processObject,
        os: { platform: function () { return processObject.platform; },
              arch: function () { return processObject.arch; },
              type: function () { return "Linux"; },
              release: function () { return machine.kernelRelease || ""; },
              hostname: function () { return machine.hostname || ""; },
              uptime: function () { return Number(clocks().machineUptime) || 0; },
              homedir: function () { return processObject.env.HOME || "/"; },
              tmpdir: function () { return "/tmp"; },
              EOL: "\n" },
        util: { inspect: rendered, format: function () {
            return Array.prototype.map.call(arguments, rendered).join(" "); } }
    };
    core.events.EventEmitter = Emitter;
    core["node:events"] = core.events;

    var modules = {};
    var loaded = {};
    function resolve(from, request) {
        var name = toText(request).replace(/^node:/, "");
        if (name.charAt(0) !== ".") { return { core: name }; }
        var parts = (from.indexOf("/") < 0 ? "" : from.slice(0, from.lastIndexOf("/"))).split("/");
        if (parts.length === 1 && parts[0] === "") { parts = []; }
        name.split("/").forEach(function (segment) {
            if (segment === "." || segment === "") { return; }
            if (segment === "..") { parts.pop(); return; }
            parts.push(segment);
        });
        var path = parts.join("/");
        var candidates = [path, path + ".js", path + ".json", path + "/index.js"];
        for (var i = 0; i < candidates.length; i++) {
            if (Object.prototype.hasOwnProperty.call(modules, candidates[i])) {
                return { path: candidates[i] };
            }
        }
        return { path: path };
    }
    function requireFrom(from) {
        return function (request) {
            var found = resolve(from, request);
            if (found.core !== undefined) {
                if (Object.prototype.hasOwnProperty.call(core, found.core)) {
                    return core[found.core];
                }
                var missing = new Error("Cannot find module '" + toText(request) + "'");
                missing.code = "MODULE_NOT_FOUND";
                throw missing;
            }
            if (Object.prototype.hasOwnProperty.call(loaded, found.path)) {
                return loaded[found.path].exports;
            }
            if (!Object.prototype.hasOwnProperty.call(modules, found.path)) {
                var absent = new Error("Cannot find module '" + toText(request) + "'");
                absent.code = "MODULE_NOT_FOUND";
                throw absent;
            }
            var source = modules[found.path];
            if (/\.json$/.test(found.path)) {
                loaded[found.path] = { exports: parse(source) };
                return loaded[found.path].exports;
            }
            var record = { exports: {}, id: found.path, filename: found.path };
            loaded[found.path] = record;
            var directory = found.path.indexOf("/") < 0 ? "" : found.path.slice(0, found.path.lastIndexOf("/"));
            var compiled = new Function(
                "exports", "require", "module", "__filename", "__dirname", source
            );
            compiled.call(record.exports, record.exports, requireFrom(found.path), record,
                          found.path, directory);
            return record.exports;
        };
    }

    globalThis.setTimeout = function (fn, delay) {
        return schedule(fn, delay, Array.prototype.slice.call(arguments, 2), false);
    };
    globalThis.setInterval = function (fn, delay) {
        return schedule(fn, delay, Array.prototype.slice.call(arguments, 2), true);
    };
    globalThis.setImmediate = function (fn) {
        return schedule(fn, 0, Array.prototype.slice.call(arguments, 1), false);
    };
    globalThis.clearTimeout = clearTimer;
    globalThis.clearInterval = clearTimer;
    globalThis.clearImmediate = clearTimer;
    globalThis.console = { log: write, info: write, debug: write, warn: write,
                           error: write, trace: write };
    globalThis.URL = Url;
    globalThis.URLSearchParams = SearchParams;
    globalThis.process = processObject;
    globalThis.require = requireFrom("");

    var driver = {
        start: function (given, entry) {
            modules = given || {};
            globalThis.require = requireFrom(entry);
            requireFrom("")("./" + entry);
            return stringify(listening());
        },
        dispatch: function (port, method, url, headers, body) {
            var serving = serverOn(port);
            if (serving === null) { return false; }
            var sent = {};
            parse(headers).forEach(function (pair) { sent[pair[0]] = pair[1]; });
            var request = new IncomingMessage(method, url, sent, body);
            var response = new ServerResponse();
            schedule(function () {
                if (body !== undefined && body !== null && body.length > 0) {
                    request.emit("data", body);
                }
                request.emit("end");
            }, 0, [], false);
            serving.emit("request", request, response);
            return true;
        },
        tick: tick
    };
    /* The ports the guest's own servers bound, in the order it bound them: 0 for a server
       it registered without naming one, which is not a port it chose. */
    function listening() {
        var bound = [];
        for (var i = 0; i < servers.length; i++) {
            if (!servers[i].listening) { continue; }
            var port = servers[i]._port;
            bound.push(port === null || port === undefined ? 0 : Number(port));
        }
        return bound;
    }
    /* The server a request addressing `port` is handed to: the one that bound THAT port,
       and otherwise one the guest registered without naming a port, which answers whatever
       the proxy addresses. `null` where the guest bound nothing there. */
    function serverOn(port) {
        var wanted = Number(port);
        for (var i = servers.length - 1; i >= 0; i--) {
            if (servers[i].listening && Number(servers[i]._port) === wanted) { return servers[i]; }
        }
        for (var j = servers.length - 1; j >= 0; j--) {
            var none = servers[j]._port === null || servers[j]._port === undefined;
            if (servers[j].listening && none) { return servers[j]; }
        }
        return null;
    }
    return driver;
})()
"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam a run that opens no connection is given: a channel nothing answers, which
    /// is how a guest that never connects is run.
    fn no_connects() -> (
        ConnectSeam,
        ::tokio::sync::mpsc::UnboundedReceiver<ConnectAsk>,
    ) {
        ::tokio::sync::mpsc::unbounded_channel()
    }

    /// One guest program of one module, which is the form a one-file guest is staged in.
    fn program(source: &str) -> Arc<GuestProgram> {
        let mut modules = ::std::collections::BTreeMap::new();
        modules.insert("index.js".to_string(), source.to_string());
        Arc::new(GuestProgram {
            modules,
            files: ::std::collections::BTreeMap::new(),
            start: GuestStart::Module {
                entry: "index.js".to_string(),
                language: GuestLanguage::JavaScript,
            },
        })
    }

    /// A GET of `path` through the endpoint, addressed to the port these programs listen
    /// on.
    fn get(path: &str) -> GuestRequest {
        GuestRequest {
            port: 8080,
            method: "GET".to_string(),
            url: path.to_string(),
            headers: vec![("host".to_string(), "microvm".to_string())],
            body: String::new(),
        }
    }

    /// RULE: a request is handed to the guest's server that bound the PORT it addresses,
    /// so a guest listening on one port answers requests addressed there and nothing
    /// addressed to a port it bound no server on; a guest with several servers answers
    /// each on its own port; and a server the guest registered without naming a port
    /// answers whatever the endpoint addresses.
    #[test]
    fn a_request_reaches_the_server_that_bound_the_port_it_addresses() {
        // (case, the guest's program, the port addressed, the body it must answer with —
        // `None` where nothing inside the MicroVM is listening there)
        let rows: Vec<(&str, &str, i32, Option<&str>)> = vec![
            (
                "the port the guest bound",
                "require('http').createServer((req, res) => res.end('on 3000')).listen(3000);",
                3_000,
                Some("on 3000"),
            ),
            (
                "a port the guest bound no server on",
                "require('http').createServer((req, res) => res.end('on 3000')).listen(3000);",
                8_080,
                None,
            ),
            (
                "the first of two servers, on its own port",
                "const http = require('http');
                 http.createServer((req, res) => res.end('first')).listen(3000);
                 http.createServer((req, res) => res.end('second')).listen(4000);",
                3_000,
                Some("first"),
            ),
            (
                "the second of two servers, on its own port",
                "const http = require('http');
                 http.createServer((req, res) => res.end('first')).listen(3000);
                 http.createServer((req, res) => res.end('second')).listen(4000);",
                4_000,
                Some("second"),
            ),
            (
                "a server the guest bound without naming a port",
                "require('http').createServer((req, res) => res.end('any port')).listen();",
                9_999,
                Some("any port"),
            ),
        ];

        for (case, source, port, expected) in rows {
            let (connects, _answers) = no_connects();
            let mut request = get("/");
            request.port = port;
            let actual = run(&program(source), &request, &connects);
            match expected {
                Some(body) => {
                    let answered = match &actual {
                        GuestOutcome::Answered(answer) => {
                            String::from_utf8_lossy(&answer.body).to_string()
                        }
                        other => format!("{other:?}"),
                    };
                    assert_eq!(
                        answered, body,
                        "case {case:?}: expected the server on port {port} to answer \
                         {body:?}, got {answered:?}"
                    );
                }
                None => {
                    let unreached = matches!(&actual, GuestOutcome::NoResponse(_));
                    assert!(
                        unreached,
                        "case {case:?}: a request addressing port {port} must reach nothing, \
                         got {actual:?}"
                    );
                }
            }
        }
    }

    /// RULE: what the endpoint answers is what the GUEST's own program produced — its
    /// status, its headers and its body — and a program that produces no response is
    /// reported as producing none.
    #[test]
    fn the_answer_is_the_one_the_guests_program_produced() {
        // (case, the guest's program, the outcome it must produce)
        let rows: Vec<(&str, &str, GuestOutcome)> = vec![
            (
                "a server answering JSON",
                r#"const http = require('http');
                   http.createServer((req, res) => {
                     res.writeHead(200, {'content-type': 'application/json'});
                     res.end(JSON.stringify({healthy: true, path: req.url}));
                   }).listen(8080);"#,
                GuestOutcome::Answered(GuestAnswer {
                    status: 200,
                    headers: vec![("content-type".to_string(), "application/json".to_string())],
                    body: br#"{"healthy":true,"path":"/health"}"#.to_vec(),
                }),
            ),
            (
                "a server answering its own status",
                r#"require('http').createServer((req, res) => {
                     res.statusCode = 404;
                     res.setHeader('content-type', 'text/plain');
                     res.end('nothing here');
                   }).listen(8080);"#,
                GuestOutcome::Answered(GuestAnswer {
                    status: 404,
                    headers: vec![("content-type".to_string(), "text/plain".to_string())],
                    body: b"nothing here".to_vec(),
                }),
            ),
            (
                "a server answering from a timer",
                r#"require('http').createServer((req, res) => {
                     setTimeout(() => res.end('later'), 2500);
                   }).listen(8080);"#,
                GuestOutcome::Answered(GuestAnswer {
                    status: 200,
                    headers: Vec::new(),
                    body: b"later".to_vec(),
                }),
            ),
            (
                "a server answering from a promise",
                r#"require('http').createServer(async (req, res) => {
                     const value = await Promise.resolve('awaited');
                     res.end(value);
                   }).listen(8080);"#,
                GuestOutcome::Answered(GuestAnswer {
                    status: 200,
                    headers: Vec::new(),
                    body: b"awaited".to_vec(),
                }),
            ),
            (
                "a program that registers no server",
                "const x = 1 + 1;",
                GuestOutcome::NoResponse(NO_LISTENER.to_string()),
            ),
            (
                "a program that raises while starting",
                "throw new Error('the guest failed to start');",
                GuestOutcome::NoResponse(
                    "the guest raised before it answered: the guest failed to start".to_string(),
                ),
            ),
            (
                "a server that never answers",
                "require('http').createServer((req, res) => {}).listen(8080);",
                GuestOutcome::NoResponse(NO_RESPONSE.to_string()),
            ),
        ];

        for (case, source, expected) in rows {
            let (connects, _answers) = no_connects();
            let actual = run(&program(source), &get("/health"), &connects);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a guest reaches no host of this machine — its runtime holds no
    /// child-process, host-library or thread module at all, and requiring one raises in
    /// the guest exactly as a missing module does.
    #[test]
    fn no_host_reaching_module_is_reachable_from_a_guest() {
        for name in [
            "child_process", "node:child_process", "worker_threads", "vm", "dns",
            "tls", "http2", "cluster", "module", "v8", "inspector", "dgram", "repl",
        ] {
            let source = format!(
                "require('http').createServer((req, res) => {{
                   try {{ require('{name}'); res.end('reached'); }}
                   catch (error) {{ res.end('refused:' + error.code); }}
                 }}).listen(8080);"
            );
            let (connects, _answers) = no_connects();
            let actual = run(&program(&source), &get("/"), &connects);
            let expected = GuestOutcome::Answered(GuestAnswer {
                status: 200,
                headers: Vec::new(),
                body: b"refused:MODULE_NOT_FOUND".to_vec(),
            });
            assert_eq!(
                actual, expected,
                "case {name:?}: expected the module to be unreachable, got {actual:?}"
            );
        }
    }

    /// RULE: every bound on a run is the host's, and a guest that does not return under
    /// one produces NO response: the step budget stops a loop with no exit (however it
    /// catches what the engine raises in it) and the engine's own heap ceiling ends a run
    /// that allocates without end.
    #[test]
    fn a_guest_that_never_returns_is_stopped_by_a_bound() {
        let rows: Vec<(&str, &str)> = vec![
            (
                "a loop with no exit",
                "require('http').createServer((req, res) => { while (true) {} })
                   .listen(8080);",
            ),
            (
                "a loop that swallows the engine's stop",
                "require('http').createServer((req, res) => {
                   while (true) { try { JSON.parse('{}'); } catch (error) {} } })
                   .listen(8080);",
            ),
            (
                "a loop that allocates",
                "require('http').createServer((req, res) => {
                   let held = []; while (true) { held.push(new Array(1000).fill('x')); } })
                   .listen(8080);",
            ),
        ];
        for (case, source) in rows {
            let (connects, _answers) = no_connects();
            let actual = run(&program(source), &get("/"), &connects);
            let stopped = matches!(&actual, GuestOutcome::NoResponse(_));
            assert!(
                stopped,
                "case {case:?}: expected the run to produce no response, got {actual:?}"
            );
        }
    }

    /// RULE: what a guest's `fs`, `process` and `os` report is its own MACHINE — the
    /// identity its MicroVM published to it, the environment its image declared and the
    /// tree it writes — and never a path, an identity or an environment of the host this
    /// emulator runs on.
    #[test]
    fn a_guest_reads_the_machine_its_microvm_published_rather_than_this_host() {
        // (case, the expression the guest answers with, what it must answer)
        let rows: Vec<(&str, &str, &str)> = vec![
            (
                "this boot's id",
                "require('fs').readFileSync('/proc/sys/kernel/random/boot_id').trim()",
                "11111111-2222-4333-8444-555555555555",
            ),
            (
                "the host name the machine publishes",
                "require('os').hostname()",
                "mvm-0123456789abcdef",
            ),
            (
                "the host name on disk",
                "require('fs').readFileSync('/etc/hostname', 'utf8').trim()",
                "mvm-0123456789abcdef",
            ),
            ("the guest process's own pid", "String(process.pid)", "1"),
            ("the platform it runs on", "process.platform", "linux"),
            ("its working directory", "process.cwd()", "/app"),
            (
                "the command it was started with",
                "require('fs').readFileSync('/proc/self/cmdline').split('\\u0000')[0]",
                "node",
            ),
            (
                "the environment its image declared",
                "Object.keys(process.env).sort().join(',')",
                "HOME,PATH,PWD",
            ),
            (
                "a file it wrote itself",
                "(function () { var fs = require('fs'); \
                   fs.writeFileSync('/data/kept', 'a value'); \
                   return fs.readFileSync('/data/kept'); })()",
                "a value",
            ),
            (
                "a path of the host",
                "String(require('fs').existsSync('/etc/passwd'))",
                "false",
            ),
            (
                "reading a path of the host",
                "(function () { try { require('fs').readFileSync('/etc/passwd'); return 'read'; } \
                   catch (error) { return error.code; } })()",
                "ENOENT",
            ),
            (
                "a relative path, against the directory its process reports",
                "String(require('fs').existsSync('Cargo.toml'))",
                "false",
            ),
            (
                "its own module, by the path its argv names",
                "String(require('fs').readFileSync(process.argv[1]).indexOf('createServer') >= 0)",
                "true",
            ),
            (
                "its own module, relative to its working directory",
                "String(require('fs').existsSync('index.js'))",
                "true",
            ),
            (
                "a path above the machine's own root",
                "String(require('fs').existsSync('/../../etc/passwd'))",
                "false",
            ),
            (
                "the architecture its image was built for",
                "process.arch",
                "arm64",
            ),
            (
                "the module the image's command starts",
                "require('path').basename(process.argv[1])",
                "index.js",
            ),
        ];

        for (case, expression, expected) in rows {
            let source = format!(
                "require('http').createServer((req, res) => {{
                   res.end(String({expression}));
                 }}).listen(8080);"
            );
            let (connects, _answers) = no_connects();
            let actual = run(&program(&source), &get("/"), &connects);
            let answered = match &actual {
                GuestOutcome::Answered(answer) => String::from_utf8_lossy(&answer.body).to_string(),
                other => format!("{other:?}"),
            };
            assert_eq!(
                answered, expected,
                "case {case:?}: expected the guest to answer {expected:?}, got {answered:?}"
            );
        }
    }

    /// RULE: the host seams the guest's runtime is built over are reachable from NOTHING
    /// in the guest: they are deleted from the global object before any of its code is
    /// compiled, so a guest that looks for them finds nothing and cannot answer, log or
    /// open a connection except through the runtime's own modules.
    #[test]
    fn no_host_seam_is_reachable_from_a_guest() {
        // Every name a host function is installed under, plus the shapes a guest would
        // reach one through if one were left behind.
        let probes: &[(&str, &str)] = &[
            ("the response sink", "globalThis.__microvmHostRespond"),
            ("the connection seam", "globalThis.__microvmHostConnect"),
            ("the log writer", "globalThis.__microvmHostLog"),
            ("the machine it was published", "globalThis.__microvmHostMachine"),
            ("the machine's clocks", "globalThis.__microvmHostClocks"),
            ("the filesystem read seam", "globalThis.__microvmHostFsRead"),
            ("the filesystem write seam", "globalThis.__microvmHostFsWrite"),
            ("the filesystem remove seam", "globalThis.__microvmHostFsRemove"),
            ("the filesystem mkdir seam", "globalThis.__microvmHostFsMkdir"),
            ("the filesystem stat seam", "globalThis.__microvmHostFsStat"),
            ("the runtime's own driver", "globalThis.__microvmRuntime"),
            (
                "the response sink by indexed lookup",
                "globalThis['__microvmHostRespond']",
            ),
        ];
        for (case, probe) in probes {
            let source = format!(
                "require('http').createServer((req, res) => {{
                   res.end(String(typeof ({probe})));
                 }}).listen(8080);"
            );
            let (connects, _answers) = no_connects();
            let actual = run(&program(&source), &get("/"), &connects);
            let expected = GuestOutcome::Answered(GuestAnswer {
                status: 200,
                headers: Vec::new(),
                body: b"undefined".to_vec(),
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {probe} to be unreachable, got {actual:?}"
            );
        }
    }

    /// RULE: the engine's own stack ceiling bounds a guest's recursion, so a guest that
    /// recurses without end ends inside the engine — as the error its code sees, or as a
    /// run that produced no response — and never on the host's stack.
    #[test]
    fn a_guest_that_recurses_without_end_is_bounded_by_the_engine() {
        let source = "require('http').createServer((req, res) => {
             const down = (depth) => down(depth + 1);
             try { down(0); res.end('returned'); }
             catch (error) { res.end('bounded'); }
           }).listen(8080);";
        let (connects, _answers) = no_connects();
        let actual = run(&program(source), &get("/"), &connects);
        let bounded = match &actual {
            GuestOutcome::Answered(answer) => answer.body == b"bounded",
            GuestOutcome::NoResponse(_) => true,
            _ => false,
        };
        assert!(
            bounded,
            "expected the recursion to be bounded inside the engine, got {actual:?}"
        );
    }

    /// The step budget is what stops a guest the clock has not caught, so it has to STAY
    /// spent: the engine keeps asking after a breach, and an answer of "keep going" there
    /// would hand the run its budget again.
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
                "the guest was stopped after {BUDGET} engine steps without answering"
            )),
            "a spent step budget is what the run reports"
        );
    }
}
