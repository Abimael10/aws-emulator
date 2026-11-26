// SPDX-License-Identifier: CC0-1.0

//! A MicroVM guest whose program is PYTHON: the interpreter it runs in, what it can reach of
//! this process, and what bounds it.
//!
//! Which engine runs a guest is the image's own declaration — a container specification
//! whose command starts `python3` on a module names a Python program
//! ([`super::guest_program::GuestLanguage::Python`]) — and such a program is RUN here, in
//! this process, by the RustPython interpreter this crate embeds. Nothing about a request is
//! synthesised: the status, the headers and the body a client reads back from the MicroVM's
//! endpoint are the ones the guest's own program produced, and the standard library's own
//! `http.server` is what parsed the request and rendered the response.
//!
//! # Containment
//!
//! The guest reaches exactly what this module installs, and nothing else. What CONTAINS the
//! interpreter itself is shared with the other service of this emulator that runs
//! caller-supplied Python (`python_guest_substrate`), so a MicroVM's guest and a function's
//! handler are narrowed by one implementation:
//!
//! * the interpreter is built with the engine crate's host-reaching surface compiled OUT
//!   (`default-features = false`, see that crate's `Cargo.toml`), so no `_ctypes` host
//!   loader, no host standard streams and no interpreter thread exist in the build;
//! * the native modules are an ALLOW-LIST ([`ALLOWED_NATIVE_MODULES`]) filtered out of the
//!   engine's own pool, so a module that reaches a socket, a subprocess, a memory map, a
//!   terminal or a host library is never registered — and is therefore reachable through no
//!   `import`, no `sys.modules` entry and no `__import__`, however the guest rebinds or
//!   deletes names. `posix`, `socket`, `socketserver` and `select` are among the absent
//!   ones, and this module supplies its OWN of each in Python ([`runtime_source`]): the
//!   environment, the working directory and the entries they reach are the MicroVM's own
//!   in-memory machine ([`super::guest_machine`]), the connections they open are decided by
//!   the emulated network ([`super::microvm_network`]), and a bound server is registered
//!   rather than given a socket of this machine;
//! * every way of opening a HOST file is replaced by a refusal — the interpreter's own file
//!   type and `open`, `io.open` and `_io.open` — and the `open` a guest calls is this
//!   module's own, which reads and writes the MicroVM's machine and no path of the host;
//! * the standard streams are the MicroVM's LOG, so nothing a guest prints reaches a stream,
//!   a descriptor or a file of this process;
//! * no built-in module can be CREATED once the interpreter is standing, so a guest that
//!   drops one of the patched modules cannot build itself an unpatched one, and no thread of
//!   this process can be started from guest code.
//!
//! # Bounds
//!
//! Every bound lives in host code the guest cannot switch off:
//!
//! * STEPS and the DEADLINE are checked by [`Bounds::step`], which the interpreter's trace
//!   hook calls DIRECTLY: the hook is a host function rather than Python, so no module
//!   attribute, global or default argument stands between a traced event and the bound, and
//!   `sys.settrace`, `sys.setprofile`, `sys.gettrace` and `sys.getprofile` are then taken
//!   away so no guest call can clear or replace it. The stop is LATCHED: once a bound is
//!   breached the request's answer is that stop, whatever the guest goes on to return or
//!   catch, and the hook arms the trace slot again on its way out, so a guest that catches
//!   the stop meets it again at its next event.
//! * MEMORY is sampled from OUTSIDE the guest — the faulted pages of the guest thread,
//!   because the interpreter exposes no allocation seam — from a baseline the host takes
//!   before each request, and a sample past the ceiling stops the run.
//! * A long NATIVE call emits no traced event, so one WATCH thread of this process reads the
//!   same clock for every live guest, samples what this process has faulted in since each
//!   run began, and latches the stop. A latched stop is still only RAISED at the next traced
//!   event, so past a grace period the watch GIVES UP on such a run instead of waiting for
//!   it: the live-process slot its thread holds goes back at once, and the thread is counted
//!   among the ones this emulator has given up on — past [`MAX_ABANDONED_GUESTS`] of those,
//!   no further interpreter is built at all, so what threads nothing can interrupt occupy is
//!   bounded whatever a client runs. The CALLER is answered either way, by the wait ceiling
//!   the endpoint holds ([`super::guest_runtime`]).
//! * RECURSION is clamped before any guest code runs and `sys.setrecursionlimit` is taken
//!   away, `time.sleep` is replaced by a wait clamped to what the request has left, and the
//!   RESPONSE and the LOG are each capped.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::rustpython_vm::builtins::PyModule;
use ::rustpython_vm::function::{ArgBytesLike, FuncArgs};
use ::rustpython_vm::{AsObject, Interpreter, PyObjectRef, PyRef, PyResult, VirtualMachine};
use ::python_guest_substrate::{
    clamp_guest_sleep, contained_interpreter, faulted_bytes, installed_refusal,
    process_faulted_bytes, refuse_builtin_module_creation, refuse_guest_threads,
    refuse_host_file_access, watch_run, WatchedRun,
};

use super::guest_machine::GuestMachine;
use super::guest_program::GuestProgram;
use super::guest_runtime::{
    capped_log_line, ConnectAsk, ConnectSeam, GuestAnswer, GuestOutcome, GuestRequest, GuestSlot,
    CONNECT_DECISION_CEILING, MAX_CONNECTS, MAX_RESPONSE_BYTES,
};
use super::microvm_network::ConnectVerdict;

/// The Python modules this emulator supplies in place of the ones it registers nothing for,
/// and the driver the host calls into the interpreter.
mod runtime_source;

/// How long one request's run may take before a bound of this module stops it.
const RUN_WALL_CLOCK: Duration = Duration::from_secs(5);

/// How many traced events one run may spend — the STEP bound beside the wall-clock one, and
/// the one that does not depend on how fast the host is.
const RUN_TRACED_EVENTS: u64 = 4_000_000;

/// How much memory one run may add to this process, sampled from the guest thread's own
/// faulted pages.
const RUN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// How often the memory sample is taken, in traced events: often enough to notice a run
/// growing, rarely enough that the reading is not the cost of the run.
const MEMORY_SAMPLE_EVERY: u64 = 2_048;

/// How deep the guest may recurse.
const RUN_RECURSION_LIMIT: usize = 512;

/// The longest a guest may ask `time.sleep` for in one call, before what the request has
/// left clamps it further.
const MAX_SLEEP_SECONDS: f64 = 60.0;

/// How long past its spent budget a run is GIVEN UP ON.
///
/// A stop this module latches is raised at the next traced event, and one NATIVE call of the
/// interpreter delivers no event however long it takes — a single large allocation or one long
/// built-in operation is inside one such call. So a run still in flight this long after its
/// budget was spent is one the host stops waiting for: its live-process slot goes back at once
/// so a guest that answers can take it, and the thread it is on is counted as given up on
/// until it ends ([`Bounds::watch`]).
const GIVE_UP_GRACE: Duration = Duration::from_secs(5);

/// How many given-up-on guest threads this process tolerates before it starts no further
/// Python guest at all.
///
/// Each one holds an interpreter and whatever its last call allocated until it returns, and
/// nothing in this process can make it return, so this is the hard ceiling on what those
/// threads can occupy. Past it a MicroVM is answered as one nothing was reached inside of
/// rather than by starting another interpreter beside them.
const MAX_ABANDONED_GUESTS: usize = 2;

/// The guest threads this process has given up on and that have not ended.
fn abandoned_guests() -> &'static AtomicUsize {
    static ABANDONED: AtomicUsize = AtomicUsize::new(0);
    &ABANDONED
}

/// Every native module this interpreter registers, out of the engine's own pool.
///
/// This list IS the containment of the native surface: a name absent from it is dropped from
/// the interpreter's module table before any guest code runs, so no guest-side import of it
/// can succeed — not through `import`, not through `importlib`, not through `__import__`, and
/// not by dropping the module from `sys.modules` and importing it again (the creation calls
/// are taken away too, see [`contain`]).
///
/// What is here is the interpreter's own machinery (the import system, the exception and
/// object model support, the bytecode and codec support) and the arithmetic, text, hashing
/// and data-structure modules the frozen standard library needs for a program to
/// `import json`, `datetime`, `hashlib`, `base64`, `random`, `re` or `uuid` — none of which
/// reaches outside this process.
///
/// Deliberately absent, so that each stays unregistered: `_socket`, `_ssl`, `select`,
/// `_posixsubprocess`, `mmap`, `_ctypes`, `fcntl`, `termios`, `resource`, `pwd`, `grp`,
/// `syslog`, `_signal`, `posix` and every other module whose calls land on the host. Where
/// one of those names a module a program imports (`posix`, `socket`, `socketserver`,
/// `select`), the module ITSELF is supplied in Python ([`runtime_source`]), so the import
/// succeeds and what it reaches is this emulator's own.
///
/// `zlib` is absent for a different reason: a stop this module latches is raised at the next
/// traced event, and one NATIVE call delivers none — so a module whose single call turns a
/// small input into an allocation the host cannot interrupt is not registered at all. The
/// frozen standard library imports it only where it guards the import, so a program that
/// names it reads the `ImportError` a build without it answers.
///
/// Two of the names here are registered but PATCHED before guest code runs, because the
/// frozen standard library needs the rest of each: `_io`, whose host file-opening calls are
/// replaced by refusals, and `time`, whose `sleep` is replaced by the clamped one.
const ALLOWED_NATIVE_MODULES: &[&str] = &[
    // The interpreter's own machinery, which nothing runs without.
    "_abc",
    "_ast",
    "_codecs",
    "_collections",
    "_functools",
    "_imp",
    "_io",
    "_operator",
    "_opcode",
    "_sre",
    "_stat",
    "_string",
    "_tokenize",
    "_types",
    "_typing",
    "_warnings",
    "_weakref",
    "atexit",
    "errno",
    "gc",
    "itertools",
    "marshal",
    "time",
    // Arithmetic, text, hashing and data structures: computation, and nothing else.
    "_bisect",
    "_blake2",
    "_contextvars",
    "_csv",
    "_datetime",
    "_hashlib",
    "_heapq",
    "_json",
    "_md5",
    "_queue",
    "_random",
    "_sha1",
    "_sha2",
    "_sha256",
    "_sha3",
    "_sha512",
    "_statistics",
    "_struct",
    "array",
    "binascii",
    "cmath",
    "math",
    "unicodedata",
];

/// The modules this emulator SUPPLIES in place of ones it does not register, under the names
/// a program imports them by. Each is this crate's own Python ([`runtime_source`]).
const SUPPLIED_MODULES: &[(&str, &str)] = &[
    ("posix", runtime_source::POSIX_SOURCE),
    ("socket", runtime_source::SOCKET_SOURCE),
    ("select", runtime_source::SELECT_SOURCE),
    ("socketserver", runtime_source::SOCKETSERVER_SOURCE),
];

/// The modules of the frozen standard library the HOST imports while standing the
/// interpreter up, before the trace hook is installed and before any guest code runs.
///
/// Each of them is a module the request path itself goes through — the server a guest binds,
/// the parser its headers are read by, the codecs its text is decoded with — so importing
/// them is this emulator's own work and is not charged to the guest's step budget, exactly as
/// a managed runtime does not charge a program for the interpreter's own start-up.
const PRE_IMPORTED_MODULES: &[&str] = &[
    "encodings",
    "os",
    "io",
    "json",
    "re",
    "traceback",
    "linecache",
    "importlib.util",
    "email.parser",
    "urllib.parse",
    "http.server",
];

/// What a guest that bound no server is reported as.
const NO_LISTENER: &str = "the guest registered no HTTP server, so nothing is listening";

/// What a guest that bound a server and wrote no response is reported as.
const NO_RESPONSE: &str = "the guest did not answer the request";

/// The guest PROCESS of one MicroVM whose program is Python: the interpreter, standing with
/// the program's own modules started in it.
///
/// It is NOT `Send`: every object here belongs to the one interpreter and the one thread it
/// was built on, so the process is reached by handing a request to that thread
/// ([`super::guest_runtime`] owns it).
pub struct PythonProcess {
    /// The interpreter itself, finalized when the process is dropped and not before.
    interpreter: Option<Interpreter>,
    /// The driver this emulator's own runtime source handed back: what one request is given
    /// to. Held OUTSIDE any module the guest can reach, so a guest that rebinds its globals
    /// cannot replace what hands it a request.
    dispatch: Option<PyObjectRef>,
    /// Where the installed trace hook is held, for the life of the process.
    trace: TraceCell,
    /// The ports the guest's servers bound when its program started, in the order it bound
    /// them. Empty means nothing is listening inside this MicroVM.
    ports: Vec<i32>,
    /// The per-request seams and the budget one request is held to.
    seams: Arc<Seams>,
}

/// Where the installed trace hook is held for the life of one process: the hook answers
/// ITSELF as each frame's local tracer, so this cell holds the hook and the hook holds this
/// cell. The process empties it once its interpreter has been finalized.
type TraceCell = ::std::rc::Rc<::std::cell::RefCell<Option<PyObjectRef>>>;

impl ::std::fmt::Debug for PythonProcess {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("PythonProcess")
            .field("ports", &self.ports)
            .finish_non_exhaustive()
    }
}

/// The interpreter is finalized when the MicroVM stops holding its process, with the objects
/// it built dropped first.
///
/// A guest `atexit` callback runs DURING that finalization, so the budget is armed again for
/// it: what runs there is guest code like any other, watched by the host's own watch, which
/// latches its deadline and gives up on a finalization that does not return.
impl Drop for PythonProcess {
    fn drop(&mut self) {
        self.dispatch.take();
        if let Some(interpreter) = self.interpreter.take() {
            self.seams.bounds.begin();
            interpreter.finalize(None);
            self.seams.bounds.finish();
        }
        self.trace.borrow_mut().take();
        // The live-process slot goes back HERE, where the process ends, rather than wherever
        // the last object the interpreter holds is freed: the modules an interpreter keeps
        // hold this process's own host seams, and those seams hold the slot — so a slot left
        // for them to drop is one this emulator never gets back.
        self.seams.bounds.release_slot();
    }
}

impl PythonProcess {
    /// Build the interpreter and START the guest's program in it.
    ///
    /// The error is what EVERY request to this process is answered with: an interpreter that
    /// could not be contained or stood up, and a program that raised or bound no server, are
    /// each a MicroVM nothing is listening inside of.
    pub(super) fn start(
        program: &GuestProgram,
        machine: Arc<GuestMachine>,
        slot: GuestSlot,
    ) -> Result<Self, GuestOutcome> {
        // A guest the host has given up on is still running, so the room it occupies is not
        // free: past the ceiling on those, no further interpreter is built at all.
        let abandoned = abandoned_guests().load(Ordering::Relaxed);
        if abandoned >= MAX_ABANDONED_GUESTS {
            return Err(GuestOutcome::Unavailable(format!(
                "the {abandoned} guest processes this emulator has given up on are still running"
            )));
        }
        let seams = Arc::new(Seams::new(machine, slot));
        let contained = Arc::new(AtomicBool::new(false));
        let interpreter = interpreter(&contained);
        // An interpreter whose native surface was not narrowed to the allow-list is never
        // handed guest code: nothing of the guest runs at all.
        if !contained.load(Ordering::SeqCst) {
            return Err(GuestOutcome::Unavailable(
                "the interpreter's native modules could not be narrowed to the allow-list"
                    .to_string(),
            ));
        }
        let trace: TraceCell = ::std::rc::Rc::new(::std::cell::RefCell::new(None));
        // The guest's own startup is held to the same budget one request is, so a program
        // that never returns does not hold this thread either.
        seams.bounds.begin();
        watch_run(&seams.bounds);
        let modules = ::serde_json::to_string(&program.modules).unwrap_or_else(|_| "{}".into());
        let started = interpreter.enter(|vm| -> Result<(PyObjectRef, Vec<i32>), GuestOutcome> {
            let driver = match stand_up(vm, &seams, &trace) {
                Ok(driver) => driver,
                Err(raised) => {
                    return Err(GuestOutcome::Unavailable(format!(
                        "the Python runtime could not be installed: {}",
                        rendered(vm, &raised)
                    )))
                }
            };
            // From here on what runs is the GUEST's: its own modules, started the way its
            // image's command starts them, and the ports they bound are what it is listening
            // on.
            let raised = driver
                .start
                .call(
                    (
                        vm.ctx.new_str(modules.clone()),
                        vm.ctx.new_str(program.entry().to_string()),
                    ),
                    vm,
                )
                .map_err(|raised| {
                    GuestOutcome::NoResponse(format!(
                        "the guest raised before it answered: {}",
                        rendered(vm, &raised)
                    ))
                })?;
            let raised: String = raised.try_into_value(vm).unwrap_or_default();
            if !raised.is_empty() {
                return Err(GuestOutcome::NoResponse(format!(
                    "the guest raised before it answered: {raised}"
                )));
            }
            let ports = driver
                .ports
                .call((), vm)
                .ok()
                .and_then(|ports| ports.try_into_value::<Vec<i32>>(vm).ok())
                .unwrap_or_default();
            Ok((driver.dispatch, ports))
        });
        for line in seams.logs() {
            ::tracing::debug!(line = ?line, "a MicroVM guest wrote a log line");
        }
        let (dispatch, ports) = match started {
            Ok(started) => started,
            Err(outcome) => {
                // Nothing of a program that did not start is kept: the interpreter is
                // finalized here rather than held for requests it cannot answer. The budget
                // stays ARMED through it, because a guest `atexit` callback its program
                // registered before it raised runs during that finalization — and the
                // live-process slot goes back once it has, because this guest is over.
                seams.bounds.begin();
                interpreter.finalize(None);
                seams.bounds.finish();
                trace.borrow_mut().take();
                seams.bounds.release_slot();
                return Err(outcome);
            }
        };
        if ports.is_empty() {
            seams.bounds.begin();
            interpreter.finalize(None);
            seams.bounds.finish();
            trace.borrow_mut().take();
            seams.bounds.release_slot();
            return Err(GuestOutcome::NoResponse(NO_LISTENER.to_string()));
        }
        // The program has started, so nothing of it is in flight any more: the watch stops
        // judging this budget until the first request arms it again.
        seams.bounds.finish();
        Ok(Self {
            interpreter: Some(interpreter),
            dispatch: Some(dispatch),
            trace,
            ports,
            seams,
        })
    }

    /// The ports the guest's own servers bound.
    pub(super) fn ports(&self) -> &[i32] {
        &self.ports
    }

    /// Hand ONE request to the guest running in this interpreter and report what it answered,
    /// with `connects` the seam the connections it opens while answering are decided over.
    pub(super) fn answer(&self, request: &GuestRequest, connects: &ConnectSeam) -> GuestOutcome {
        // Every bound is spent per request, and the seams point at THIS request's client.
        self.seams.begin(connects.clone());
        let (Some(interpreter), Some(dispatch)) =
            (self.interpreter.as_ref(), self.dispatch.as_ref())
        else {
            return GuestOutcome::Unavailable(
                "the MicroVM's guest process is no longer running".to_string(),
            );
        };
        let headers = ::serde_json::to_string(&request.headers).unwrap_or_else(|_| "[]".to_string());
        let dispatched = interpreter.enter(|vm| {
            let answered = dispatch.call(
                (
                    vm.ctx.new_int(request.port),
                    vm.ctx.new_str(request.method.clone()),
                    vm.ctx.new_str(request.url.clone()),
                    vm.ctx.new_str(headers),
                    vm.ctx.new_bytes(request.body.clone().into_bytes()),
                ),
                vm,
            );
            match answered {
                Ok(answered) => Ok(answered.try_into_value::<String>(vm).unwrap_or_default()),
                Err(raised) => Err(rendered(vm, &raised)),
            }
        });
        for line in self.seams.logs() {
            ::tracing::debug!(line = ?line, "a MicroVM guest wrote a log line");
        }
        // The run is no longer in flight, so the watch stops judging it and a thread the host
        // had given up on stops being counted as one.
        self.seams.bounds.finish();
        // A bound that was breached is what the request came to, whatever the guest went on
        // to answer or catch.
        if let Some(breach) = self.seams.bounds.breach() {
            return GuestOutcome::NoResponse(breach);
        }
        match (self.seams.answer(), dispatched) {
            (Some(answer), _) => GuestOutcome::Answered(answer),
            (None, Ok(raised)) if !raised.is_empty() => GuestOutcome::NoResponse(format!(
                "the guest raised before it answered: {raised}"
            )),
            (None, Err(raised)) => GuestOutcome::NoResponse(format!(
                "the guest raised before it answered: {raised}"
            )),
            (None, Ok(_)) => GuestOutcome::NoResponse(NO_RESPONSE.to_string()),
        }
    }
}

/// The three entry points this emulator's own runtime source hands the host.
struct Driver {
    /// Starts the guest's program.
    start: PyObjectRef,
    /// Answers the ports its servers bound.
    ports: PyObjectRef,
    /// Hands it one request.
    dispatch: PyObjectRef,
}

/// The interpreter itself: the frozen standard library and the ALLOW-LISTED native modules,
/// and nothing else ([`contained_interpreter`], shared with the other service that runs
/// caller-supplied Python).
///
/// `contained` reports whether the narrowing really happened: a guest is refused rather than
/// run if it did not.
fn interpreter(contained: &Arc<AtomicBool>) -> Interpreter {
    // This service supplies the modules it does not allow in PYTHON ([`SUPPLIED_MODULES`]),
    // not as native definitions, so there is none to register here.
    contained_interpreter(ALLOWED_NATIVE_MODULES, |_vm| Vec::new(), contained)
}

/// Install everything the guest sees of the host, evaluate this emulator's own runtime source
/// in the interpreter and hand back the driver it defines.
///
/// The TRACE HOOK is installed LAST, once the modules the request path goes through are
/// standing, so none of this emulator's own work is charged to the guest's budget.
fn stand_up(vm: &VirtualMachine, seams: &Arc<Seams>, trace: &TraceCell) -> PyResult<Driver> {
    let modules = vm.sys_module.as_object().get_attr("modules", vm)?;
    // Every allow-listed native module is created ONCE, here, so each is the single patched
    // object `sys.modules` holds from now on.
    for name in ALLOWED_NATIVE_MODULES {
        let _ = vm.import(*name, 0);
    }
    // The host seam, under the name the supplied modules import it by. It is not in the
    // native module table, so there is no second module of that name to be had.
    modules.set_item(
        runtime_source::HOST_MODULE,
        host_module(vm, seams)?.into(),
        vm,
    )?;
    refuse_host_file_access(vm, NO_HOST_FILE, REFUSED_GUEST)?;
    // The modules this emulator supplies in place of the ones it registers nothing for, in
    // order: `posix` first, because `os` builds itself on it and the host states what it
    // reports before anything imports it.
    for (name, source) in SUPPLIED_MODULES {
        let module = supplied_module(vm, name, source)?;
        modules.set_item(*name, module.clone(), vm)?;
        if *name == "posix" {
            install_machine_identity(vm, &module, &seams.machine)?;
        }
        if *name == "socket" {
            module.set_attr(
                "_hostname",
                vm.ctx.new_str(seams.machine.hostname.clone()),
                vm,
            )?;
        }
    }
    // The files a guest opens, and the standard streams it writes through, both installed
    // before any module of the standard library is imported: an import of the one and a log
    // line of the other each reach what this emulator holds and nothing of the host.
    install_machine_files(vm)?;
    install_log_streams(vm)?;
    // What the interpreter reports as BUILT IN, which is what the standard library reads to
    // decide which operating-system module to build itself on: stated before `os` is
    // imported, because that is where `os` reads it.
    declare_builtin_modules(vm)?;
    // The codec registry and the modules the request path itself goes through, imported by
    // the HOST while nothing of the guest's has run.
    for name in PRE_IMPORTED_MODULES {
        vm.import(*name, 0)?;
    }
    let driver = evaluate_runtime(vm)?;
    contain(vm, seams)?;
    install_trace_hook(vm, seams, trace)?;
    Ok(driver)
}

/// Compile and run one module this emulator supplies, as the module object it becomes.
fn supplied_module(vm: &VirtualMachine, name: &str, source: &str) -> PyResult<PyObjectRef> {
    let held = vm.ctx.new_dict();
    let module = vm.new_module(name, held.clone(), None);
    let code = vm
        .compile(
            source,
            ::rustpython_vm::compiler::Mode::Exec,
            format!("<microvm-{name}>"),
        )
        .map_err(|error| {
            vm.new_runtime_error(format!(
                "the {name} this emulator supplies does not compile: {error}"
            ))
        })?;
    vm.run_code_obj(
        code,
        ::rustpython_vm::scope::Scope::with_builtins(None, held, vm),
    )?;
    Ok(module.into())
}

/// Evaluate this emulator's own runtime source and take the three entry points out of it.
fn evaluate_runtime(vm: &VirtualMachine) -> PyResult<Driver> {
    let scope = vm.new_scope_with_builtins();
    let code = vm
        .compile(
            runtime_source::RUNTIME_SOURCE,
            ::rustpython_vm::compiler::Mode::Exec,
            "<microvm-runtime>".to_owned(),
        )
        .map_err(|error| {
            vm.new_runtime_error(format!("the MicroVM Python runtime does not compile: {error}"))
        })?;
    vm.run_code_obj(code, scope.clone())?;
    Ok(Driver {
        start: scope.globals.get_item("__microvm_start", vm)?,
        ports: scope.globals.get_item("__microvm_ports", vm)?,
        dispatch: scope.globals.get_item("__microvm_dispatch", vm)?,
    })
}

/// Install what the MACHINE published to its guest on the module `os` is built on: the
/// environment, the working directory and the identity of its single process.
///
/// The HOST states each of them, from the MicroVM's own machine, before anything imports `os`
/// — which is where `os` takes its environment mapping from — so what a guest reads is its
/// MicroVM's and nothing of the process this emulator runs in. The POSIX entries are bytes,
/// which is the form `os` decodes a POSIX environment from.
fn install_machine_identity(
    vm: &VirtualMachine,
    posix: &PyObjectRef,
    machine: &Arc<GuestMachine>,
) -> PyResult<()> {
    let environ = vm.ctx.new_dict();
    for (name, value) in &machine.env {
        let key: PyObjectRef = vm.ctx.new_bytes(name.as_bytes().to_vec()).into();
        let held: PyObjectRef = vm.ctx.new_bytes(value.as_bytes().to_vec()).into();
        environ.set_item(&*key, held, vm)?;
    }
    posix.set_attr("environ", environ, vm)?;
    posix.set_attr(
        "_cwd",
        vm.ctx.new_str(super::guest_machine::GUEST_WORKING_DIRECTORY),
        vm,
    )?;
    posix.set_attr("_pid", vm.ctx.new_int(super::guest_machine::GUEST_PID), vm)?;
    Ok(())
}

/// Install the `open` a guest calls: the one that reads and writes the MicroVM's own machine.
///
/// It goes on `builtins` AFTER every way of opening a host file has been refused
/// ([`refuse_host_file_access`]), so the only file a guest can open at all is one of its own
/// machine. The installation is CHECKED: a guest left with an `open` that is not this
/// emulator's runs nothing.
fn install_machine_files(vm: &VirtualMachine) -> PyResult<()> {
    let module = supplied_module(vm, "_microvm_files", runtime_source::FILES_SOURCE)?;
    let opener = module.get_attr("open", vm)?;
    let builtins = vm.builtins.as_object();
    builtins.set_attr("open", opener.clone(), vm)?;
    installed_refusal(vm, builtins, "open", &opener, "builtins", REFUSED_GUEST)
}

/// Install the standard streams: the MicroVM's own log, in place of this process's streams.
fn install_log_streams(vm: &VirtualMachine) -> PyResult<()> {
    let module = supplied_module(vm, "_microvm_streams", runtime_source::STREAMS_SOURCE)?;
    let stream = module.get_attr("_LogStream", vm)?;
    for name in ["stdout", "stderr"] {
        let held = stream.call((vm.ctx.new_str(name),), vm)?;
        vm.sys_module.as_object().set_attr(name, held, vm)?;
    }
    Ok(())
}

/// Take away every way of opening a HOST file, and what a guest reads when it tries.
const NO_HOST_FILE: &str = "a MicroVM guest in this emulator cannot open a file of the host";

/// What a guest that tries to start a thread of this process reads.
const NO_GUEST_THREAD: &str =
    "a MicroVM guest in this emulator starts no thread of this process";

/// What a guest that tries to build itself a native module reads.
const NO_BUILTIN_MODULE: &str =
    "a MicroVM guest in this emulator cannot create a built-in module";

/// What the host refuses when a containment patch of this module did not take effect.
const REFUSED_GUEST: &str = "no guest is run in this interpreter";

/// Close what is left of the host once the runtime is standing: the thread calls, the
/// unclamped sleep, the recursion limit and the creation of a built-in module — each of them
/// the shared substrate's own patch, read back where it is installed.
fn contain(vm: &VirtualMachine, seams: &Arc<Seams>) -> PyResult<()> {
    let _ = refuse_guest_threads(vm, NO_GUEST_THREAD, REFUSED_GUEST)?;
    let sleeping = Arc::clone(seams);
    clamp_guest_sleep(vm, REFUSED_GUEST, move |seconds| {
        sleeping.bounds.wait(seconds);
    })?;
    ::python_guest_substrate::clamp_recursion(vm, RUN_RECURSION_LIMIT, REFUSED_GUEST)?;
    refuse_builtin_module_creation(vm, NO_BUILTIN_MODULE, REFUSED_GUEST)
}

/// Declare what the interpreter reports as BUILT IN: the allow-listed native modules and the
/// ones this emulator supplies, and nothing else.
///
/// The standard library reads this tuple to decide which operating-system module to build
/// itself on, so it is stated before `os` is imported — and what it names is exactly what is
/// registered, so no program reads a host-reaching module into existence from it.
fn declare_builtin_modules(vm: &VirtualMachine) -> PyResult<()> {
    let names: Vec<PyObjectRef> = SUPPLIED_MODULES
        .iter()
        .map(|(name, _)| *name)
        .chain(ALLOWED_NATIVE_MODULES.iter().copied())
        .map(|name| vm.ctx.new_str(name).into())
        .collect();
    vm.sys_module
        .as_object()
        .set_attr("builtin_module_names", vm.ctx.new_tuple(names), vm)
}

/// Install the trace hook the step, deadline and memory bounds are checked from, then take
/// away every way the guest could clear or replace it.
fn install_trace_hook(vm: &VirtualMachine, seams: &Arc<Seams>, held: &TraceCell) -> PyResult<()> {
    let answered = ::std::rc::Rc::clone(held);
    let stepping = Arc::clone(seams);
    let arming: ::std::rc::Rc<::std::cell::RefCell<Option<PyObjectRef>>> =
        ::std::rc::Rc::new(::std::cell::RefCell::new(None));
    let armed = ::std::rc::Rc::clone(&arming);
    let arming_now = ::std::rc::Rc::new(::std::cell::Cell::new(false));
    let hook = vm.new_function(
        "__microvm_trace",
        move |_args: FuncArgs, vm: &VirtualMachine| {
            if let Some(stop) = stepping.bounds.step() {
                // The slot again, with this same hook, so a guest that catches what follows
                // is traced — and stopped — at its next event too.
                if !arming_now.replace(true) {
                    let arm = armed.borrow().clone();
                    let hook = answered.borrow().clone();
                    if let (Some(arm), Some(hook)) = (arm, hook) {
                        let _ = arm.call((hook,), vm);
                    }
                    arming_now.set(false);
                }
                return Err(vm.new_runtime_error(stop));
            }
            let held = answered.borrow();
            Ok(held.clone().unwrap_or_else(|| vm.ctx.none()))
        },
    );
    let hook: PyObjectRef = hook.into();
    *held.borrow_mut() = Some(hook.clone());
    let setter = vm.sys_module.as_object().get_attr("settrace", vm)?;
    *arming.borrow_mut() = Some(setter.clone());
    setter.call((hook,), vm)?;
    // With these gone, no guest call can reach the interpreter's trace slot at all: the hook
    // that spends the budget is the only one there will be.
    for name in ["settrace", "setprofile", "gettrace", "getprofile"] {
        let _ = vm.sys_module.as_object().del_attr(name, vm);
        if vm.sys_module.as_object().get_attr(name, vm).is_ok() {
            return Err(vm.new_runtime_error(format!(
                "sys.{name} could not be taken away, so no guest is run in this interpreter"
            )));
        }
    }
    Ok(())
}

/// The host functions the guest's modules are built over, and the whole of what guest code
/// can call of this process: the MicroVM's log, its machine's files, ONE outbound connection
/// decided by the emulated network, a sleep clamped to what the request has left, and the
/// response to the request in flight.
///
/// Nothing else of the host is reachable from guest code, and no bound of the request is
/// reachable from it either.
fn host_module(vm: &VirtualMachine, seams: &Arc<Seams>) -> PyResult<PyRef<PyModule>> {
    let module = vm.new_module(runtime_source::HOST_MODULE, vm.ctx.new_dict(), None);

    let logging = Arc::clone(seams);
    set_function(vm, &module, "log", move |args, vm| {
        let text: String = args.bind(vm)?;
        logging.log(&text);
        Ok(vm.ctx.none())
    });

    let answering = Arc::clone(seams);
    set_function(vm, &module, "respond", move |args, vm| {
        let (status, headers, body): (i32, String, ArgBytesLike) = args.bind(vm)?;
        answering.respond(status, &headers, &body.borrow_buf());
        Ok(vm.ctx.none())
    });

    let connecting = Arc::clone(seams);
    set_function(vm, &module, "connect", move |args, vm| {
        let (host, port): (String, i32) = args.bind(vm)?;
        let verdict = connecting.connect(&host, port);
        let local: PyObjectRef = match verdict.local_address {
            Some(address) => vm.ctx.new_str(address).into(),
            None => vm.ctx.none(),
        };
        let local_port: PyObjectRef = vm
            .ctx
            .new_int(i32::from(verdict.local_port.unwrap_or_default()))
            .into();
        Ok(vm
            .ctx
            .new_tuple(vec![
                vm.ctx.new_bool(verdict.connected).into(),
                local,
                local_port,
            ])
            .into())
    });

    let sleeping = Arc::clone(seams);
    set_function(vm, &module, "sleep", move |args, vm| {
        let seconds: f64 = args.bind(vm)?;
        sleeping.bounds.wait(seconds);
        Ok(vm.ctx.none())
    });

    let reading = Arc::clone(seams);
    set_function(vm, &module, "fs_read", move |args, vm| {
        let path: String = args.bind(vm)?;
        Ok(match reading.machine.read(&path) {
            Some(held) => vm.ctx.new_bytes(held.into_bytes()).into(),
            None => vm.ctx.none(),
        })
    });

    let writing = Arc::clone(seams);
    set_function(vm, &module, "fs_write", move |args, vm| {
        let (path, data): (String, ArgBytesLike) = args.bind(vm)?;
        // The machine holds TEXT files, so bytes that are not text are no file of it.
        let wrote = match String::from_utf8(data.borrow_buf().to_vec()) {
            Ok(text) => writing.machine.write(&path, &text, false),
            Err(_) => false,
        };
        Ok(vm.ctx.new_bool(wrote).into())
    });

    let stating = Arc::clone(seams);
    set_function(vm, &module, "fs_stat", move |args, vm| {
        let path: String = args.bind(vm)?;
        let held = stating.machine.stat(&path);
        if !held.exists {
            return Ok(vm.ctx.none());
        }
        Ok(vm
            .ctx
            .new_tuple(vec![
                vm.ctx.new_bool(held.directory).into(),
                vm.ctx.new_int(held.size).into(),
            ])
            .into())
    });

    let making = Arc::clone(seams);
    set_function(vm, &module, "fs_mkdir", move |args, vm| {
        let path: String = args.bind(vm)?;
        Ok(vm
            .ctx
            .new_bool(making.machine.make_directories(&path))
            .into())
    });

    let removing = Arc::clone(seams);
    set_function(vm, &module, "fs_remove", move |args, vm| {
        let path: String = args.bind(vm)?;
        Ok(vm.ctx.new_bool(removing.machine.remove(&path)).into())
    });

    Ok(module)
}

/// Put one host function on `module` under `name`.
fn set_function<F>(vm: &VirtualMachine, module: &PyRef<PyModule>, name: &'static str, function: F)
where
    F: Fn(FuncArgs, &VirtualMachine) -> PyResult<PyObjectRef> + Send + Sync + 'static,
{
    let callable = vm.new_function(name, function);
    let _ = module.as_object().set_attr(name, callable, vm);
}

/// How an exception is reported: the error's own class and message, as the guest worded them.
fn rendered(vm: &VirtualMachine, raised: &::rustpython_vm::builtins::PyBaseExceptionRef) -> String {
    let mut text = String::new();
    if vm.write_exception(&mut text, raised).is_err() {
        return "an exception that could not be rendered".to_string();
    }
    let line = text
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or("an exception that could not be rendered");
    line.trim().to_string()
}

/// The per-request seams the guest's runtime is built over, and the budget one request is
/// held to: the host state one request writes into and the next request resets.
struct Seams {
    /// The response the guest answered THIS request with.
    answered: Mutex<Option<GuestAnswer>>,
    /// The log lines it wrote answering it.
    collected: Mutex<Vec<String>>,
    /// The seam this request's outbound connections are decided over, and `None` between
    /// requests — where a connection the guest opens is one nothing carried.
    connects: Mutex<Option<ConnectSeam>>,
    /// How many connections THIS request has opened.
    opened: AtomicU64,
    /// The budget this request is held to.
    bounds: Arc<Bounds>,
    /// The machine the guest's `posix`, its files and its host name answer from.
    machine: Arc<GuestMachine>,
}

impl Seams {
    /// The seams of one guest process, before its first request, holding the live-process
    /// slot that process occupies.
    fn new(machine: Arc<GuestMachine>, slot: GuestSlot) -> Self {
        Self {
            answered: Mutex::new(None),
            collected: Mutex::new(Vec::new()),
            connects: Mutex::new(None),
            opened: AtomicU64::new(0),
            bounds: Arc::new(Bounds::new(slot)),
            machine,
        }
    }

    /// Start one request: nothing of the request before it is left in the seams, and the
    /// budget is the new request's own.
    fn begin(&self, connects: ConnectSeam) {
        *self.answered.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.collected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
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

    /// The lines the guest wrote, taken out of the seams as they are read.
    fn logs(&self) -> Vec<String> {
        ::std::mem::take(&mut *self.collected.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Record one log line, under this module's ceilings.
    fn log(&self, message: &str) {
        let mut held = self.collected.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(line) = capped_log_line(held.len(), message) {
            held.push(line);
        }
    }

    /// Keep the response the guest wrote, under the response cap.
    fn respond(&self, status: i32, headers: &str, body: &[u8]) {
        let mut held = self.answered.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_some() {
            // A guest that answers twice has answered once: the first response is the one the
            // client reads, exactly as a server that has already flushed its headers cannot
            // take them back.
            return;
        }
        let mut body = body.to_vec();
        body.truncate(MAX_RESPONSE_BYTES);
        *held = Some(GuestAnswer {
            status: u16::try_from(status).unwrap_or(200),
            headers: ::serde_json::from_str::<Vec<(String, String)>>(headers).unwrap_or_default(),
            body,
        });
    }

    /// Ask the host to decide ONE outbound connection of the request in flight.
    fn connect(&self, host: &str, port: i32) -> ConnectVerdict {
        let taken = self.opened.fetch_add(1, Ordering::Relaxed);
        if taken >= MAX_CONNECTS {
            return ConnectVerdict::unreached();
        }
        // The request in flight is what decides a connection; a guest that opens one between
        // requests has nothing to carry it.
        let Some(seam) = self
            .connects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return ConnectVerdict::unreached();
        };
        let (reply, answer) = ::std::sync::mpsc::channel();
        if seam
            .send(ConnectAsk {
                host: host.to_string(),
                port,
                reply,
            })
            .is_err()
        {
            return ConnectVerdict::unreached();
        }
        answer
            .recv_timeout(CONNECT_DECISION_CEILING)
            .unwrap_or_else(|_| ConnectVerdict::unreached())
    }
}

/// The budget one REQUEST is held to, as the trace hook spends it, and the latch that says
/// which bound ended it.
///
/// One guest process answers request after request in the same interpreter, so the budget is
/// reset at the start of each ([`Self::begin`]): what a guest spent answering one client is
/// never charged to the next.
#[derive(Debug)]
struct Bounds {
    /// The instant this budget measures its clock from, fixed for the life of the process so
    /// the deadline can be read and written without a lock.
    origin: Instant,
    /// When the request in flight must be over, in milliseconds since [`Self::origin`].
    deadline: AtomicU64,
    /// How many traced events one request starts with, which a breach reports.
    budget: u64,
    /// How many traced events are left.
    events: AtomicU64,
    /// What the guest thread had faulted in when this request's own code began.
    memory_baseline: AtomicU64,
    /// What this PROCESS had faulted in at that same instant, which is what the host's own
    /// watch reads a growing run against from outside the guest ([`Self::watch`]).
    process_baseline: AtomicU64,
    /// How much memory one request may add to this process.
    memory_ceiling: u64,
    /// Whether a call of the engine is in flight, which is what the watch judges: between
    /// requests there is nothing to stop and nothing to give up on.
    in_flight: AtomicBool,
    /// Whether the host has GIVEN UP on the thread this run is on ([`GIVE_UP_GRACE`]), which
    /// is counted in [`abandoned_guests`] until the call returns.
    abandoned: AtomicBool,
    /// The live-process slot the guest this budget belongs to occupies, given back at once
    /// where the host gives up on it.
    slot: GuestSlot,
    /// Set once the clock ran out.
    timed_out: AtomicBool,
    /// Set once the events ran out.
    events_spent: AtomicBool,
    /// Set once the memory sample passed the ceiling.
    over_memory: AtomicBool,
}

impl Bounds {
    /// A budget of the events, the clock and the memory one request is held to.
    fn new(slot: GuestSlot) -> Self {
        Self::with_budget(RUN_TRACED_EVENTS, RUN_MEMORY_BYTES, slot)
    }

    /// A budget of `budget` traced events and `memory_ceiling` bytes starting now — the one
    /// constructor, so a budget small enough to spend in a test is spent through exactly the
    /// path a request's is.
    fn with_budget(budget: u64, memory_ceiling: u64, slot: GuestSlot) -> Self {
        let bounds = Self {
            origin: Instant::now(),
            deadline: AtomicU64::new(0),
            budget,
            events: AtomicU64::new(budget),
            memory_baseline: AtomicU64::new(0),
            process_baseline: AtomicU64::new(0),
            memory_ceiling,
            in_flight: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            slot,
            timed_out: AtomicBool::new(false),
            events_spent: AtomicBool::new(false),
            over_memory: AtomicBool::new(false),
        };
        bounds.begin();
        bounds
    }

    /// Start one request's budget: its whole clock, its whole event budget and a memory
    /// baseline of its own, with no breach of the request before it left latched.
    fn begin(&self) {
        let wall_clock = u64::try_from(RUN_WALL_CLOCK.as_millis()).unwrap_or(u64::MAX);
        self.deadline
            .store(self.elapsed().saturating_add(wall_clock), Ordering::Relaxed);
        self.events.store(self.budget, Ordering::Relaxed);
        self.memory_baseline.store(faulted_bytes(), Ordering::Relaxed);
        self.process_baseline
            .store(process_faulted_bytes(), Ordering::Relaxed);
        self.timed_out.store(false, Ordering::Relaxed);
        self.events_spent.store(false, Ordering::Relaxed);
        self.over_memory.store(false, Ordering::Relaxed);
        self.in_flight.store(true, Ordering::Relaxed);
    }

    /// The call of the engine this budget was spent on has RETURNED.
    ///
    /// Nothing is in flight to stop any more, and a thread the host had given up on is no
    /// longer one: it came back, so it stops counting against the guests this process will
    /// start. Its live-process slot stays where it was given back to — what runs on this
    /// thread from here on is a guest the store no longer holds a slot for.
    fn finish(&self) {
        self.in_flight.store(false, Ordering::Relaxed);
        if self.abandoned.swap(false, Ordering::Relaxed) {
            abandoned_guests().fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Give the live-process slot of the guest this budget belongs to back, because that
    /// guest is over: the thread it ran on is ending and nothing of it will answer again.
    fn release_slot(&self) {
        self.slot.release();
    }

    /// How long this budget's own clock has been running, in milliseconds.
    fn elapsed(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Spend ONE traced event, answering the stop the request is over with where a bound has
    /// been breached. Every answer latches, so a breach survives a guest that catches the
    /// exception the hook raises in it.
    fn step(&self) -> Option<String> {
        if let Some(latched) = self.breach() {
            return Some(latched);
        }
        let before = self
            .events
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                Some(left.saturating_sub(1))
            })
            .unwrap_or_default();
        if before <= 1 {
            self.events_spent.store(true, Ordering::Relaxed);
            return self.breach();
        }
        if before % MEMORY_SAMPLE_EVERY == 0 {
            // The sample the hook takes of its OWN thread, and the clock beside it: both are
            // system calls, so they are taken at this interval rather than per event — the
            // host's own watch reads both for every live run anyway ([`Self::watch`]).
            if self.over_ceiling() {
                self.over_memory.store(true, Ordering::Relaxed);
            }
            if self.elapsed() >= self.deadline.load(Ordering::Relaxed) {
                self.timed_out.store(true, Ordering::Relaxed);
            }
            return self.breach();
        }
        None
    }

    /// What the HOST's own watch does with a live run, from a thread of its own and without
    /// the guest reaching a traced event: it latches the stop its clock is due, it latches the
    /// stop its MEMORY is due — sampled as what this process has faulted in beyond what it
    /// held when the run began, because the faulted pages of the run's own thread can be read
    /// only from that thread — and past [`GIVE_UP_GRACE`] it gives up on the run altogether.
    ///
    /// Giving up is the part that does not depend on the guest: a stop can only be RAISED at
    /// the next traced event, and one native call of the interpreter delivers none, so the
    /// host instead takes back what that thread holds of this emulator — its live-process slot
    /// goes back at once, so a guest that answers can take it — and counts the thread among
    /// the ones it has given up on, which is what stops further interpreters from being built
    /// beside it ([`MAX_ABANDONED_GUESTS`]).
    fn watch(&self) {
        if !self.in_flight.load(Ordering::Relaxed) {
            return;
        }
        let elapsed = self.elapsed();
        let deadline = self.deadline.load(Ordering::Relaxed);
        if elapsed >= deadline {
            self.timed_out.store(true, Ordering::Relaxed);
        }
        if self.process_grown() > self.memory_ceiling {
            self.over_memory.store(true, Ordering::Relaxed);
        }
        let grace = u64::try_from(GIVE_UP_GRACE.as_millis()).unwrap_or(u64::MAX);
        if elapsed >= deadline.saturating_add(grace)
            && !self.abandoned.swap(true, Ordering::Relaxed)
        {
            abandoned_guests().fetch_add(1, Ordering::Relaxed);
            self.slot.release();
            ::tracing::debug!(
                "a MicroVM guest still inside its engine after its budget was spent was given \
                 up on, and the live-process slot it held went back"
            );
        }
    }

    /// How much this PROCESS has faulted in beyond what it held when the run began.
    fn process_grown(&self) -> u64 {
        process_faulted_bytes().saturating_sub(self.process_baseline.load(Ordering::Relaxed))
    }

    /// Whether what this thread holds beyond the request's baseline is past the ceiling.
    fn over_ceiling(&self) -> bool {
        faulted_bytes().saturating_sub(self.memory_baseline.load(Ordering::Relaxed))
            > self.memory_ceiling
    }

    /// Wait for `seconds`, but never past what the request has left.
    fn wait(&self, seconds: f64) {
        if !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        let asked = Duration::from_secs_f64(seconds.min(MAX_SLEEP_SECONDS));
        let left = self
            .deadline
            .load(Ordering::Relaxed)
            .saturating_sub(self.elapsed());
        let waited = asked.min(Duration::from_millis(left));
        if !waited.is_zero() {
            ::std::thread::sleep(waited);
        }
    }

    /// What the breached bound is reported as, or `None` when none was breached.
    ///
    /// A pure read of the LATCHES: it takes no reading of the clock or of the memory, because
    /// the trace hook asks it on every traced event. What takes a reading is the hook's own
    /// sample, every [`MEMORY_SAMPLE_EVERY`] events, and the host's watch, every
    /// [`python_guest_substrate::WATCH_INTERVAL`].
    fn breach(&self) -> Option<String> {
        if self.timed_out.load(Ordering::Relaxed) {
            return Some(format!(
                "the guest was stopped after {} seconds without answering",
                RUN_WALL_CLOCK.as_secs()
            ));
        }
        if self.events_spent.load(Ordering::Relaxed) {
            return Some(format!(
                "the guest was stopped after {} interpreter steps without answering",
                self.budget
            ));
        }
        if self.over_memory.load(Ordering::Relaxed) {
            return Some(format!(
                "the guest was stopped after holding more than {} bytes of memory",
                self.memory_ceiling
            ));
        }
        None
    }
}

/// A thread the host gave up on that ENDS is no longer one it is waiting for, so the count
/// that bounds them goes back wherever the budget it was spent against is dropped.
impl Drop for Bounds {
    fn drop(&mut self) {
        if self.abandoned.swap(false, Ordering::Relaxed) {
            abandoned_guests().fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The host's own watch samples one run of a MicroVM's guest the same way it samples a run of
/// any caller-supplied Python ([`python_guest_substrate::watch_run`]); what a sample MEANS is
/// this service's own ([`Bounds::watch`]).
impl WatchedRun for Bounds {
    fn watch(&self) {
        Self::watch(self);
    }
}
