// SPDX-License-Identifier: CC0-1.0

#![forbid(unsafe_code)]

//! The CONTAINED Python interpreter the services of this emulator that run caller-supplied
//! Python share, and the readings their bounds are judged on.
//!
//! Two services run such code: one runs a function's own handler, the other runs the guest a
//! MicroVM image holds. What they run it IN is the same thing — an interpreter whose native
//! surface is an allow-list, whose host file, thread and module-creation calls are refusals
//! the host reads back, and whose runs are sampled from outside the guest — so that half
//! lives here rather than in a copy each. A containment patch or a reading fixed here is
//! fixed for both; what stays in each service is only what its own API decides: which
//! modules it supplies on the import path, what its guest's filesystem and network are, and
//! what one of its runs may spend.
//!
//! Everything here is the HOST's side of the boundary. No function in this crate is
//! reachable from guest code, and each containment patch is READ BACK after it is installed
//! ([`installed_refusal`]): a patch that was applied and did not take is a hole no later
//! reading would report, so it is an error at standup and the caller refuses the run.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use ::rustpython_vm::builtins::PyModuleDef;
use ::rustpython_vm::function::FuncArgs;
use ::rustpython_vm::{
    AsObject, Interpreter, PyObject, PyObjectRef, PyResult, Settings, VirtualMachine,
};

/// Build an interpreter whose native surface is `allowed` and whose standard library is the
/// FROZEN one, with `supplied` the module definitions the caller registers in place of ones
/// it does not allow.
///
/// `contained` reports whether the narrowing really happened: the module table is reached
/// through the one mutable borrow the initialisation hook has, and a caller that reads
/// `false` must refuse the run rather than hand guest code an interpreter whose native
/// surface is not the allow-list.
///
/// A name absent from `allowed` is dropped from the interpreter's own table before any guest
/// code runs, so no guest-side import of it can succeed — not through `import`, not through
/// `importlib`, not through `__import__`, and not by dropping the module from `sys.modules`
/// and importing it again, which [`refuse_builtin_module_creation`] closes.
pub fn contained_interpreter<F>(
    allowed: &'static [&'static str],
    supplied: F,
    contained: &Arc<AtomicBool>,
) -> Interpreter
where
    F: Fn(&VirtualMachine) -> Vec<&'static PyModuleDef> + Send + Sync + 'static,
{
    let mut settings = Settings::default();
    // No host path is searched for a module: the standard library is the frozen one and the
    // caller's own modules are resolved by the finder its runtime installs.
    settings.path_list = Vec::new();
    settings.ignore_environment = true;
    settings.isolated = true;
    settings.write_bytecode = false;
    settings.quiet = true;
    // Nothing of the HOST is processed on the way up: no signal handler is installed for this
    // process, and `site` — which exists to read host directories — is not imported.
    settings.install_signal_handlers = false;
    settings.import_site = false;
    let contained = Arc::clone(contained);
    Interpreter::with_init(settings, move |vm| {
        let from_pool: Vec<&'static PyModuleDef> = ::rustpython_stdlib::stdlib_module_defs(&vm.ctx)
            .into_iter()
            .filter(|def| allowed.contains(&def.name.as_str()))
            .collect();
        let supplied = supplied(vm);
        let Some(state) = ::std::rc::Rc::get_mut(&mut vm.state) else {
            return;
        };
        state.module_defs.retain(|name, _| allowed.contains(name));
        for def in from_pool.into_iter().chain(supplied) {
            state.module_defs.insert(def.name.as_str(), def);
        }
        state.frozen.extend(::rustpython_pylib::FROZEN_STDLIB);
        contained.store(true, Ordering::SeqCst);
    })
}

/// Read `attribute` of `holder` back and answer whether it IS the refusal that was just put
/// there — the host's own confirmation that a containment patch took effect.
///
/// `refused_run` is what the caller refuses when it did not, named as that caller's own run.
pub fn installed_refusal(
    vm: &VirtualMachine,
    holder: &PyObject,
    attribute: &'static str,
    refusing: &PyObjectRef,
    held_by: &str,
    refused_run: &str,
) -> PyResult<()> {
    let held = holder.get_attr(attribute, vm)?;
    if held.is(refusing) {
        return Ok(());
    }
    Err(vm.new_runtime_error(format!(
        "the refusal this emulator installs on {held_by}.{attribute} did not take effect, so \
         {refused_run}"
    )))
}

/// Take away every way of opening a HOST file: the TYPE that opens one, and each of the names
/// a Python program reaches it through.
///
/// The refusal is put on the type's own construction, not only on the names: a type that has
/// been created stays in the interpreter's type graph and is reachable by traversal from
/// `object`, so replacing the name alone would leave the type that opens a file one
/// `__subclasses__` walk away. With `__new__` and `__init__` refusing, an instance obtained
/// however it was reached opens nothing. The rest of `_io` — the in-memory streams the
/// standard library is built on — is left exactly as it is, and a caller that serves files of
/// its own puts its own `open` on `builtins` after this.
pub fn refuse_host_file_access(
    vm: &VirtualMachine,
    refusal: &'static str,
    refused_run: &str,
) -> PyResult<()> {
    // `io` is loaded FIRST, so the standard library's own stream classes are bound from the
    // real module before anything on it is replaced.
    vm.import("io", 0)?;
    let refusing = refusing_function(vm, "open", refusal);
    // The TYPE, before the names it is read off are replaced: its construction is what opens
    // a host file, so that is where the refusal goes — and it is READ BACK on both slots like
    // every other patch here, because this is the one that closes the traversal from
    // `object.__subclasses__()` to the type itself. A build of the interpreter that registers
    // no such type at all (this one registers none: `_io` carries no `FileIO`) leaves nothing
    // in the graph to reach, which is the one case where the ABSENCE is the containment — and
    // that it holds is asserted from guest code, by a walk of `object.__subclasses__()` in the
    // tests of the services that run a guest.
    if let Ok(raw_file) = vm.import("_io", 0)?.get_attr("FileIO", vm) {
        for slot in ["__new__", "__init__"] {
            raw_file.set_attr(slot, refusing.clone(), vm)?;
            installed_refusal(vm, &raw_file, slot, &refusing, "_io.FileIO", refused_run)?;
        }
    }
    for name in ["_io", "io"] {
        let module = vm.import(name, 0)?;
        for attribute in ["open", "open_code", "FileIO"] {
            module.set_attr(attribute, refusing.clone(), vm)?;
            installed_refusal(vm, &module, attribute, &refusing, name, refused_run)?;
        }
    }
    let builtins = vm.builtins.as_object();
    for attribute in ["open", "open_code"] {
        builtins.set_attr(attribute, refusing.clone(), vm)?;
        installed_refusal(vm, builtins, attribute, &refusing, "builtins", refused_run)?;
    }
    Ok(())
}

/// Take away every call that would start a HOST thread, and answer whether there was a thread
/// module to take them away from.
///
/// A real guest thread would run outside every bound the host holds — a step budget, a stop
/// latch and a memory sample all belong to one thread — so none can be started.
pub fn refuse_guest_threads(
    vm: &VirtualMachine,
    refusal: &'static str,
    refused_run: &str,
) -> PyResult<bool> {
    let Ok(threads) = vm.import("_thread", 0) else {
        return Ok(false);
    };
    let refusing = refusing_runtime_function(vm, "start_new_thread", refusal);
    for attribute in [
        "start_new_thread",
        "start_new",
        "start_joinable_thread",
        "interrupt_main",
        "_set_sentinel",
    ] {
        threads.set_attr(attribute, refusing.clone(), vm)?;
        installed_refusal(vm, &threads, attribute, &refusing, "_thread", refused_run)?;
    }
    Ok(true)
}

/// Take away the CREATION of a built-in module, so a guest that drops one of the patched
/// modules cannot build itself an unpatched replacement.
///
/// `is_builtin` stays, because the import system asks it on every import; what is taken away
/// is the creation itself.
pub fn refuse_builtin_module_creation(
    vm: &VirtualMachine,
    refusal: &'static str,
    refused_run: &str,
) -> PyResult<()> {
    let importer = vm.import("_imp", 0)?;
    let refusing = vm.new_function("create_builtin", move |_args: FuncArgs, vm: &VirtualMachine| {
        Err::<PyObjectRef, _>(
            vm.new_import_error(refusal.to_owned(), vm.ctx.new_str("<builtin>")),
        )
    });
    let refusing: PyObjectRef = refusing.into();
    for name in [
        "create_builtin",
        "exec_builtin",
        "create_dynamic",
        "exec_dynamic",
    ] {
        importer.set_attr(name, refusing.clone(), vm)?;
        installed_refusal(vm, &importer, name, &refusing, "_imp", refused_run)?;
    }
    Ok(())
}

/// Clamp how deep the guest may recurse, and take away the call that would raise it.
pub fn clamp_recursion(vm: &VirtualMachine, limit: usize, refused_run: &str) -> PyResult<()> {
    let setter = vm.sys_module.as_object().get_attr("setrecursionlimit", vm)?;
    setter.call((limit,), vm)?;
    // Checked, like every containment patch here: a call left in place would let a guest
    // raise the limit the host clamped, so one that could not be taken away refuses the run.
    let _ = vm.sys_module.as_object().del_attr("setrecursionlimit", vm);
    if vm
        .sys_module
        .as_object()
        .get_attr("setrecursionlimit", vm)
        .is_ok()
    {
        return Err(vm.new_runtime_error(format!(
            "sys.setrecursionlimit could not be taken away, so {refused_run}"
        )));
    }
    Ok(())
}

/// Replace the interpreter's own `sleep` with `wait`, which the caller clamps to what its own
/// run has left, on the single clock module `sys.modules` holds.
///
/// A blocking wait the guest asked for cannot outlast the span its caller allows it, and the
/// unclamped primitive is reachable nowhere: the replacement is read back like every other
/// containment patch.
pub fn clamp_guest_sleep<F>(vm: &VirtualMachine, refused_run: &str, wait: F) -> PyResult<()>
where
    F: Fn(f64) + Send + Sync + 'static,
{
    let clamped = vm.new_function("sleep", move |args: FuncArgs, vm: &VirtualMachine| {
        let seconds: f64 = args.bind(vm)?;
        wait(seconds);
        Ok::<PyObjectRef, ::rustpython_vm::builtins::PyBaseExceptionRef>(vm.ctx.none())
    });
    let clamped: PyObjectRef = clamped.into();
    let clock = vm.import("time", 0)?;
    clock.set_attr("sleep", clamped.clone(), vm)?;
    installed_refusal(vm, &clock, "sleep", &clamped, "time", refused_run)
}

/// One host function that refuses, under the name a Python program reaches it by: the error
/// a platform answers for a call it cannot make at all.
fn refusing_function(
    vm: &VirtualMachine,
    name: &'static str,
    refusal: &'static str,
) -> PyObjectRef {
    vm.new_function(name, move |_args: FuncArgs, vm: &VirtualMachine| {
        Err::<PyObjectRef, _>(vm.new_os_error(refusal.to_owned()))
    })
    .into()
}

/// The same, as the error a program reads for a RUNTIME that cannot do the thing at all,
/// which is what starting a thread of this process is.
fn refusing_runtime_function(
    vm: &VirtualMachine,
    name: &'static str,
    refusal: &'static str,
) -> PyObjectRef {
    vm.new_function(name, move |_args: FuncArgs, vm: &VirtualMachine| {
        Err::<PyObjectRef, _>(vm.new_runtime_error(refusal.to_owned()))
    })
    .into()
}

/// The size of a page of this process's memory, which a faulted-page count is scaled by.
const PAGE_BYTES: u64 = 4_096;

/// How many bytes of memory THIS THREAD has faulted in, as the host reports it.
///
/// The interpreter exposes no allocation seam, so the memory a run holds is sampled from
/// outside it, on the thread the run happens on. The count never falls, so the reading is an
/// UPPER bound on what the run holds — the side to err on for a figure a bound is judged on.
#[must_use]
pub fn faulted_bytes() -> u64 {
    page_bytes(::nix::sys::resource::UsageWho::RUSAGE_THREAD)
}

/// How many bytes of memory THIS PROCESS has faulted in, as the host reports it.
///
/// It is what a watch outside the run reads a growing run against: the faulted pages of a
/// run's own thread can be read only from that thread, and a watch exists for the run that is
/// not reaching the host at all.
#[must_use]
pub fn process_faulted_bytes() -> u64 {
    page_bytes(::nix::sys::resource::UsageWho::RUSAGE_SELF)
}

/// The faulted pages `who` reports, in bytes.
fn page_bytes(who: ::nix::sys::resource::UsageWho) -> u64 {
    match ::nix::sys::resource::getrusage(who) {
        Ok(usage) => {
            let pages = usage
                .minor_page_faults()
                .saturating_add(usage.major_page_faults());
            u64::try_from(pages).unwrap_or(0).saturating_mul(PAGE_BYTES)
        }
        Err(_) => 0,
    }
}

/// A run the host's own watch samples while it lasts.
///
/// What [`WatchedRun::watch`] does with a sample is the caller's: a run's budget is its
/// service's own, and so is what breaching it means.
pub trait WatchedRun: Send + Sync + 'static {
    /// Sample this run from OUTSIDE it. Called on the watch's own thread, every
    /// [`WATCH_INTERVAL`], for as long as the run is held anywhere else.
    fn watch(&self);
}

/// How often the watch samples a live run.
pub const WATCH_INTERVAL: Duration = Duration::from_millis(50);

/// Have the host's own watch sample `run` while it lasts.
///
/// A long NATIVE call of the interpreter emits no traced event, so a bound checked only from
/// inside the guest is one the guest decides the timing of. The watch is ONE thread for the
/// whole process: it holds a WEAK reference to each run, so a run that ended simply falls out
/// of the list, and it starts with the first run that asks to be watched.
pub fn watch_run<T: WatchedRun>(run: &Arc<T>) {
    let mut watched = watched_runs().lock().unwrap_or_else(PoisonError::into_inner);
    watched.retain(|held| held.strong_count() > 0);
    watched.push(Arc::downgrade(run) as Weak<dyn WatchedRun>);
    drop(watched);
    start_watch_thread();
}

/// Every run the watch is sampling.
fn watched_runs() -> &'static Mutex<Vec<Weak<dyn WatchedRun>>> {
    static WATCHED: ::std::sync::OnceLock<Mutex<Vec<Weak<dyn WatchedRun>>>> =
        ::std::sync::OnceLock::new();
    WATCHED.get_or_init(|| Mutex::new(Vec::new()))
}

/// Start the one watch thread of this process, if it is not already running.
fn start_watch_thread() {
    static STARTED: ::std::sync::OnceLock<()> = ::std::sync::OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = ::std::thread::Builder::new()
            .name("python-guest-watch".to_string())
            .spawn(|| loop {
                ::std::thread::sleep(WATCH_INTERVAL);
                // The live runs are copied out under the lock and sampled outside it, so a
                // sample never holds the list against the next run that starts.
                let live: Vec<Arc<dyn WatchedRun>> = {
                    let mut watched =
                        watched_runs().lock().unwrap_or_else(PoisonError::into_inner);
                    watched.retain(|held| held.strong_count() > 0);
                    watched.iter().filter_map(Weak::upgrade).collect()
                };
                for run in live {
                    run.watch();
                }
            });
    });
}
