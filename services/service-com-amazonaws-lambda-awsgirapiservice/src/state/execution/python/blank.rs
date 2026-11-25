// SPDX-License-Identifier: CC0-1.0

//! An execution environment STANDING WITH NO FUNCTION IN IT YET: the half of the build that
//! reads no function's configuration, and the adoption that makes it one function's.
//!
//! Building an execution environment is two kinds of work. One kind is the same whatever
//! function is served in it — the interpreter, the narrowing of its native surface to the
//! allow-list, the host surface the guest sees of this emulator and the evaluation of this
//! emulator's own runtime source ([`super::stand_up`]) — and it is by far the larger of the
//! two. The other kind is one function's own: what the environment IS, the variables its
//! invocations run with, how deep their calls may nest, which release of the language its
//! runtime names, and the INIT phase that imports the handler's own module
//! ([`super::initialize`]).
//!
//! Only the second kind can wait for a function. So the first is stood up BEFORE there is an
//! invocation to serve — while the function it will serve is being provisioned
//! ([`super::super::warm_pool`]) — and an invocation that arrives afterwards ADOPTS the
//! standing one: it binds its own log, bounds and seam onto it ([`super::Session::bind`]),
//! applies its function's configuration and runs its own initialization, which is the
//! `Init Duration` of a cold start and nothing besides. An invocation that finds nothing
//! standing still does both halves itself ([`super::Prepared::start`]), so what this module
//! changes is who PAYS for the first half and never what a run observes.
//!
//! A blank environment holds an interpreter of this host's, so it is a THREAD like every
//! other environment is and it is held under the same bounds: the pool that keeps it decides
//! how many stand and for how long.

use super::*;

/// An execution environment built as far as no function has to be known, waiting to be
/// adopted by the first invocation that needs one.
///
/// It is NOT `Send`, exactly as [`super::Prepared`] is not: every object in it belongs to the
/// one interpreter and the one thread it was built on, so it is adopted on the thread that
/// built it and the invocation travels to that thread.
pub(in crate::state::execution) struct Blank {
    /// The interpreter itself, finalized by [`Self::discard`] or, once a function has adopted
    /// it, by the environment it becomes.
    interpreter: Interpreter,
    /// What the invocation being served brings, which the host functions and the trace hook
    /// read. It carries the provisioning's own log and bounds until an invocation binds its
    /// own ([`Self::adopt`]).
    session: Arc<Session>,
    /// Where the installed trace hook is held, for the life of the environment.
    held: TraceCell,
    /// The entry points the environment is driven through, and the mappings its invocations'
    /// variables are written into.
    entries: Entries,
}

impl Blank {
    /// Stand the function-independent half of an environment up, on this thread.
    ///
    /// `Err` carries why no environment could be stood up at all, as the sentence the
    /// invocation that was waiting for one is answered with ([`super::Prepared::start`]) —
    /// there is no function and no invocation here, so there is nothing else it could be.
    pub(in crate::state::execution) fn stand_up(clock: &Clock) -> Result<Self, String> {
        let budget = provisioning_budget();
        let capture = Arc::new(Mutex::new(Capture::new(budget.log_bytes)));
        let bounds = Arc::new(Bounds::new(&budget, clock.clone()));
        // No seam, no durable execution and no mount: an environment no invocation has
        // adopted is serving none, so there is no caller whose identity a call could be made
        // as, no execution to checkpoint on and no file system of a function's to attach.
        let session = Session::new(&capture, &bounds, None, None, &[]);
        // What is in flight from here until the environment stands is this emulator's own
        // work and none of any function's, so it is bounded by the span that work is allowed
        // and by no span of a run's.
        bounds.begin_provisioning();
        // And the host's own watch over that span, from before the interpreter is built, as
        // the invocation-path build has it ([`super::Prepared::start`]): the span is read by
        // a thread of the HOST's, because the trace hook that reads it from inside the
        // interpreter is not installed until the environment stands.
        watch_run(&bounds);
        let contained = Arc::new(AtomicBool::new(false));
        let interpreter = interpreter(&contained);
        // An environment only stands when the native surface really was narrowed to the
        // allow-list. If it could not be, none is kept at all: an uncontained interpreter is
        // never handed guest code.
        if !contained.load(Ordering::SeqCst) {
            interpreter.finalize(None);
            return Err(UNCONTAINED_INTERPRETER.to_string());
        }
        // The hook the bounds are spent through, held HERE: it stays installed and answering
        // until the interpreter has been finalized, because finalization runs the guest's own
        // `atexit` callbacks and those frames are traced like every other guest frame.
        let held: TraceCell = ::std::rc::Rc::new(::std::cell::RefCell::new(None));
        let stood = interpreter.enter(|vm| {
            stand_up(vm, &session, &held).map_err(|exception| formatted_exception(vm, &exception))
        });
        match stood {
            Ok(entries) => Ok(Self {
                interpreter,
                session,
                held,
                entries,
            }),
            Err(why) => {
                // Nothing of a build that did not stand is kept: the interpreter is finalized
                // and the hook let go, exactly as they are after a run ([`tear_down`]).
                tear_down(interpreter, None, held);
                Err(format!(
                    "an execution environment of this emulator could not be stood up: {why}"
                ))
            }
        }
    }

    /// Make this environment the one `request`'s function is served in, and run the INIT
    /// phase of that function in it.
    ///
    /// The invocation's own log, bounds, SDK seam, durable journal and mounts are bound onto
    /// the environment FIRST, so everything that follows — the function's configuration and
    /// the module-level code of its own deployment package — is spent against this
    /// invocation's budget and written into this invocation's log, exactly as it is when one
    /// call builds the whole environment.
    ///
    /// `Err` carries what an initialization that failed is answered with. No environment is
    /// kept in that case.
    pub(in crate::state::execution) fn adopt(
        self,
        request: &Run<'_>,
        clock: &Clock,
    ) -> Result<Prepared, Box<Finished>> {
        let Self {
            interpreter,
            session,
            held,
            entries,
        } = self;
        let capture = Arc::new(Mutex::new(Capture::new(request.budget.log_bytes)));
        let bounds = Arc::new(Bounds::new(&request.budget, clock.clone()));
        session.bind(
            &capture,
            &bounds,
            request.aws,
            request.durable,
            request.file_systems,
        );
        // Applying the function's own configuration is still this emulator's work on its own
        // host, like the interpreter was, so it is bounded by the span that work is allowed
        // until the INIT phase begins ([`super::initialize`]).
        bounds.begin_provisioning();
        // The host's own watch over the initialization, sampling the clock from OUTSIDE the
        // guest for as long as it lasts ([`super::watch_run`]).
        watch_run(&bounds);
        // The document the environment is prepared from: the mounts this invocation's
        // function brings, read after the mounts were attached above.
        let input = match runtime_input(request, &session) {
            Ok(input) => input,
            Err(error) => {
                // This environment serves no invocation after all, so nothing of it is kept:
                // the same release every other path that gives one up performs
                // ([`tear_down`]).
                tear_down(interpreter, Some(entries), held);
                return Err(Box::new(settled(
                    Completion::Internal(error),
                    &bounds,
                    &capture,
                    Duration::ZERO,
                    true,
                )));
            }
        };
        let initialized = interpreter.enter(|vm| {
            match initialize(vm, request, &input, &entries, &bounds) {
                Ok(initialized) => initialized,
                Err(exception) => {
                    Initialized::Answered(Completion::Internal(formatted_exception(vm, &exception)))
                }
            }
        });
        match initialized {
            Initialized::Ready => Ok(Prepared {
                interpreter,
                session,
                held,
                entries,
                initializing: ::std::cell::RefCell::new(Some((capture, bounds))),
                // Taken on the environment's OWN thread, which is the one every invocation
                // served in it runs on: what it has faulted in beyond this is what the
                // environment holds of the host.
                faulted_at_build: faulted_bytes(),
            }),
            Initialized::Answered(completion) => {
                // Nothing of a failed initialization is kept: the interpreter is finalized
                // here, exactly as it is after a run ([`tear_down`]).
                tear_down(interpreter, Some(entries), held);
                Err(Box::new(settled(
                    completion,
                    &bounds,
                    &capture,
                    Duration::ZERO,
                    true,
                )))
            }
        }
    }

    /// Tear this environment down, once no invocation will adopt it.
    ///
    /// The interpreter is FINALIZED rather than only dropped, for the reason
    /// [`super::Prepared::finalize`] states, and against bounds of its own — a span of this
    /// emulator's own work, because an environment no function ever adopted ran no guest code
    /// and has no invocation's budget to be torn down against.
    pub(in crate::state::execution) fn discard(self) {
        let budget = provisioning_budget();
        let capture = Arc::new(Mutex::new(Capture::new(budget.log_bytes)));
        let bounds = Arc::new(Bounds::new(&budget, Clock::host()));
        bounds.begin_provisioning();
        self.session.bind(&capture, &bounds, None, None, &[]);
        watch_run(&bounds);
        let Self {
            interpreter,
            session: _,
            held,
            entries,
        } = self;
        tear_down(interpreter, Some(entries), held);
    }
}

/// Release everything one environment holds of this host, in the one order that releases all
/// of it: the interpreter's own objects, then the interpreter itself, then the trace hook.
///
/// It is ONE function because every path that gives an environment up has to do all three —
/// a build that did not stand, an initialization that answered, a document that could not be
/// built for the invocation adopting it, and an environment nothing will adopt — and a path
/// that omitted a step would strand what that step releases for the life of this process.
/// `entries` is `None` for an environment whose entry points were never resolved.
///
/// The interpreter is FINALIZED rather than only dropped, for the reason
/// [`super::Prepared::finalize`] states. The hook is let go LAST, because the hook and the
/// cell holding it hold each other: emptying that cell is what lets the one this environment
/// installed go.
fn tear_down(interpreter: Interpreter, entries: Option<Entries>, held: TraceCell) {
    // The entry points and the mappings this environment wrote every invocation's variables
    // through are the interpreter's objects, so they go before it is finalized.
    drop(entries);
    interpreter.finalize(None);
    held.borrow_mut().take();
}

/// What standing an environment up may spend, before there is an invocation to charge it to.
///
/// Nothing of a function is read while a blank environment is built, so there is no
/// `MemorySize`, `Timeout` or `EphemeralStorage` of one to bound it by: the figures are the
/// ones a function has when it states none of them, and what really bounds this work is the
/// span this emulator may spend standing an environment up
/// ([`Bounds::begin_provisioning`]). The invocation that adopts the environment brings its
/// own bounds and has them bound onto it before a line of its code is read
/// ([`Blank::adopt`]).
fn provisioning_budget() -> Budget {
    super::super::run_budget(
        crate::state::DEFAULT_TIMEOUT_SECONDS,
        crate::state::DEFAULT_MEMORY_SIZE_MB,
        crate::state::DEFAULT_EPHEMERAL_STORAGE_MB,
        false,
    )
}
