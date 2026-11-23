// SPDX-License-Identifier: CC0-1.0

//! The APPSYNC_JS sandbox one `EvaluateCode` call is answered from.
//!
//! `EvaluateCode` "evaluates the given code and returns the response … For APPSYNC_JS
//! runtimes, the code defines the request and response functions", so the code is an ES
//! module whose `request`/`response` export is CALLED over the caller's context, and the
//! call's `evaluationResult` is the JSON of whatever that call returned. Nothing is read
//! out of the code's TEXT: it is compiled and executed by the QuickJS engine embedded in
//! this process (`rquickjs`), the same engine a channel namespace's event handlers run in
//! ([`crate::service::event_api::handlers`]), and the result, the logs, the stash and the
//! out-errors this module reports are the ones that run produced.
//!
//! # What the code can reach
//!
//! A FRESH engine per call, so nothing of one evaluation is reachable from the next: no
//! global, no module instance and no value outlives the call that made it. Inside that
//! engine the code reaches the language and the host functions of this module's own (the
//! log, the appended error, the id mint, the DynamoDB marshalling, the relational-database
//! statement helpers and the record of the `extensions` calls it made), and a module resolver
//! that answers with the TWO specifiers AppSync's own resolvers import
//! ([`crate::service::event_api::handlers::UTILS_MODULE`] and [`RDS_UTILS_MODULE`]) — so an
//! `import` of anything
//! else fails to resolve rather than reaching a host path. Plain QuickJS registers no
//! filesystem, process, environment, network, host-library or thread API, and this module
//! adds none: `dyn-load` is left out of the build (see this crate's `Cargo.toml`).
//!
//! # What bounds it
//!
//! Every bound lives in host code the evaluated code cannot reach, and the first three are
//! checked by the engine's own interrupt handler as it works:
//!
//! * a WALL CLOCK ([`RUN_WALL_CLOCK`]), so code that never returns is stopped and the
//!   caller is still answered;
//! * a STEP budget ([`RUN_ENGINE_CHECKS`]), so a tight loop that makes no progress is
//!   stopped even where the clock has not run out;
//! * a MEMORY ceiling ([`RUN_MEMORY_BYTES`]) and a STACK ceiling ([`RUN_STACK_BYTES`]),
//!   both the engine's own, so an allocation or a recursion past them ends the run inside
//!   the engine;
//! * a RESULT ceiling ([`MAX_ANSWER_BYTES`]), the modelled maximum length of
//!   `evaluationResult`, and ceilings on how many log lines ([`MAX_LOG_ENTRIES`]), how
//!   long a line ([`MAX_LOG_ENTRY_BYTES`]) and how many out-errors ([`MAX_OUT_ERRORS`]) one
//!   evaluation may produce, so one call cannot make an unbounded answer;
//! * a CALLER WAIT ceiling ([`CALLER_WAIT_CEILING`]) held outside the engine, so the
//!   request is answered even where a stretch of engine-internal work outlasts the run's
//!   own clock — the engine can only be stopped at a check it makes;
//! * a count of the engines this process runs AT ONCE ([`MAX_SANDBOX_ENGINES`]), so the
//!   threads and the memory this operation can hold are bounded however many requests
//!   arrive: each run takes one slot for the life of the thread that owns its engine, and a
//!   run the host has already answered costs that one slot and nothing else — in particular
//!   no thread of the executor serving other requests ([`in_sandbox`]).
//!
//! Which bound was breached is LATCHED in host state ([`Bounds`]) rather than read back out
//! of the engine, so code that catches the exception the engine raises in it cannot make
//! the run report a result: once the latch is set, the run's answer is the stop. The engine
//! raises that stop at every check it makes from then on, so a run that swallows one is
//! stopped at the next — which is why a stopped run reports the BOUND it breached rather
//! than the host giving up on it (`tests/all/evaluate_code.rs` holds it to that). The bounds
//! are this sandbox's own rather than the event-handler path's, because what they bound is
//! one API call the caller waits on, and the breach is reported TO that caller.
//!
//! The run happens on a thread of the sandbox's own ([`in_sandbox`]), never on the async
//! executor serving the request.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::rquickjs::function::Func;
use ::rquickjs::loader::{ImportAttributes, Loader, Resolver};
use ::rquickjs::{CatchResultExt, Context, Ctx, Function, Module, Runtime, Value};
use ::serde_json::Value as Json;

use crate::service::event_api::handlers::UTILS_MODULE;
use crate::state::{now_nanos, splitmix64};

/// The `@aws-appsync/utils/rds` module: the statement builders a resolver over a relational
/// database writes its request with, as their own module. The `#[path]` is what points this
/// declaration beside this file, for the reason the one on this module itself states
/// (`crate::ops::evaluate_code`).
#[path = "appsync_js/rds_module.rs"]
mod rds_module;
use rds_module::{RDS_HOST_FUNCTION, RDS_UTILS_MODULE, RDS_UTILS_MODULE_SOURCE};

/// How long one evaluation may run before it is stopped.
///
/// A bound of this emulator's own, not a documented AppSync timeout: it exists so one
/// `EvaluateCode` call cannot hold a request — or a thread — open for longer than an
/// evaluation takes.
const RUN_WALL_CLOCK: Duration = Duration::from_secs(2);

/// How long the CALLER waits for a run at most, which is longer than the run's own clock
/// ([`RUN_WALL_CLOCK`]) by the margin it takes the engine to notice that clock.
///
/// A ceiling of the HOST's, held outside the engine: the engine can only be stopped at a
/// check it makes, so a stretch of engine-internal work can outlast the run's own clock.
/// This is what answers the request anyway.
const CALLER_WAIT_CEILING: Duration = Duration::from_secs(5);

/// How many engine checks one evaluation may spend. The engine asks the interrupt handler
/// as it works, so a loop that allocates nothing and calls nothing still runs the count
/// down and is stopped by it.
const RUN_ENGINE_CHECKS: u64 = 20_000_000;

/// The memory ceiling one evaluation's engine is built with.
const RUN_MEMORY_BYTES: usize = 32 * 1024 * 1024;

/// The stack ceiling one evaluation's engine is built with, which is what ends a runaway
/// recursion.
///
/// Well below the stack of the blocking thread the run happens on (`tokio`'s own, which is
/// measured in megabytes), so a recursion past this raises INSIDE the engine and is
/// reported as a failed evaluation rather than overflowing the host thread.
const RUN_STACK_BYTES: usize = 512 * 1024;

/// The largest answer one evaluation may hand back, in bytes: the modelled maximum length
/// of `EvaluationResult` (`@length 0..65536`), which is the longest result the response can
/// carry at all. The stash is held to the same ceiling, for the same reason.
pub const MAX_ANSWER_BYTES: usize = 65_536;

/// How many log lines one evaluation's `logs` may carry.
pub const MAX_LOG_ENTRIES: usize = 100;

/// The largest one log line may be, in bytes.
pub const MAX_LOG_ENTRY_BYTES: usize = 4_096;

/// How many errors one evaluation may append to `outErrors`.
pub const MAX_OUT_ERRORS: usize = 100;

/// How many `extensions.*` calls one evaluation's record may carry ([`ExtensionCall`]), so
/// code calling one in a loop cannot make an unbounded answer.
pub const MAX_EXTENSION_CALLS: usize = 16;

/// The `extensions` calls this runtime records for its host, by the names the module
/// exports them under.
///
/// `setSubscriptionFilter` and `setSubscriptionInvalidationFilter` are stated by a
/// SUBSCRIPTION field's request handler, which runs when a subscriber subscribes, and
/// `invalidateSubscriptions` by a mutation's response handler; each takes one argument, and
/// what the host does with it is the plane's own
/// (`crate::service::graphql_api::subscription_filters`).
pub const SET_SUBSCRIPTION_FILTER: &str = "setSubscriptionFilter";
/// See [`SET_SUBSCRIPTION_FILTER`].
pub const SET_SUBSCRIPTION_INVALIDATION_FILTER: &str = "setSubscriptionInvalidationFilter";
/// See [`SET_SUBSCRIPTION_FILTER`].
pub const INVALIDATE_SUBSCRIPTIONS: &str = "invalidateSubscriptions";

/// The `function` names a request may ask for: "The function within the code to be
/// evaluated. If provided, the valid values are request and response"
/// (`EvaluateCodeRequest$function`).
pub const EVALUATED_FUNCTIONS: &[&str] = &["request", "response"];

/// The function evaluated when the request names none: the first of the two the code
/// defines, which is the one a resolver runs first.
pub const DEFAULT_FUNCTION: &str = "request";

/// The `errorType` of a [`CodeFault`] for a module that does not export a FUNCTION the
/// runtime would call: "the code defines the request and response functions"
/// (`EvaluateCode`), so a module exporting a function for only one of them — or exporting
/// the name as something that is not a function — defines no mapping the runtime could run.
pub const MISSING_MAPPING_EXPORT: &str = "MISSING_MAPPING_EXPORT";

/// The `errorType` of a [`CodeFault`] for code the engine could not READ, whether it was
/// read to be run ([`run`]) or read to be stored ([`compile_faults`]).
///
/// The `errorType` member is free text ("Examples include, but aren't limited to:
/// `LINT_ERROR`, `PARSER_ERROR`", `CodeError$errorType`), and recorded behaviour names a
/// code fault the compiler could not get past this one.
pub const PARSE_ERROR: &str = "PARSE_ERROR";

/// The `span` a fault carries when the engine does not report one: "The span/length of the
/// error. Defaults to -1 if unknown" (`CodeErrorLocation$span`).
pub const UNKNOWN_SPAN: i32 = -1;

/// The unit name the evaluated code is compiled under, so a stack frame taken inside it
/// names the caller's own code rather than a host path. It is also the unit a DEFINITION's
/// own diagnostic names (`crate::state::code_definition`), so one piece of caller code is
/// named one way wherever it is read.
pub const CODE_UNIT: &str = "code.js";

/// The member of a context that carries the stash: "An object available inside each
/// resolver and function handler", which the evaluated code may read and write and which
/// the response reports as it stands after the run.
pub const STASH_MEMBER: &str = "stash";

/// The member of a context that carries the arguments the field's selection was made with,
/// and the SHORT name the same map is read under: an APPSYNC_JS `ctx` carries one arguments
/// map, reachable by either name, which is how a resolver's own context is built
/// (`crate::service::graphql_api`).
pub const ARGUMENTS_MEMBER: &str = "arguments";
/// See [`ARGUMENTS_MEMBER`].
pub const ARGUMENTS_SHORT_MEMBER: &str = "args";

/// One `extensions.*` call the evaluated code made: which function it called, and the
/// argument it called it with as the JSON text the engine rendered.
///
/// The call is RECORDED rather than acted on inside the engine: what it asks for belongs to
/// the real-time plane, which reads the record after the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCall {
    /// The name the module exports the called function under: one of
    /// [`SET_SUBSCRIPTION_FILTER`], [`SET_SUBSCRIPTION_INVALIDATION_FILTER`] or
    /// [`INVALIDATE_SUBSCRIPTIONS`].
    pub name: String,
    /// The argument the call carried, as JSON text.
    pub argument: String,
}

impl ExtensionCall {
    /// The argument as a document, or `Json::Null` for a call the engine rendered as
    /// nothing readable.
    pub fn argument(&self) -> Json {
        ::serde_json::from_str(&self.argument).unwrap_or(Json::Null)
    }
}

/// One fault found in the code itself, as a `CodeError` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFault {
    /// The kind of fault: [`PARSE_ERROR`] or [`MISSING_MAPPING_EXPORT`].
    pub error_type: &'static str,
    /// The user-presentable error, as the engine worded it.
    pub value: String,
    /// The line the fault sits on, or 0 when the engine reported none.
    pub line: i32,
    /// The column the fault sits on, or 0 when the engine reported none.
    pub column: i32,
    /// The span of the fault, or [`UNKNOWN_SPAN`] when it is unknown.
    pub span: i32,
}

/// What evaluating the code came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The evaluated function returned: the JSON text of the value it returned, as the
    /// engine rendered it (`null` for a function that returned nothing).
    Returned(String),
    /// The evaluated function ended the step it belongs to through `runtime.earlyReturn`:
    /// the JSON text of the value that call was handed (`null` for a call handed nothing).
    ///
    /// The value is the step's answer exactly as a plain return's is, so a caller that wants
    /// only the value reads the two the same way; a caller that RUNS a resolver step reads
    /// them apart, because an early return is the request function saying the step is over —
    /// the data source it names is not called and its response function does not run.
    ReturnedEarly(String),
    /// The evaluated code did not return: it raised, or its module body did. The message is
    /// the error's own, and `error_type` the `errorType` the raise named — `util.error`'s
    /// second argument, which an error of a resolved field reports as its own `errorType` —
    /// or `None` for a raise that named none.
    ///
    /// `refused_the_field` is set only by `util.unauthorized`, which REFUSES the field being
    /// resolved rather than reporting an error the code worded: the caller that resolves a
    /// field words that refusal itself. An error of the code's own carries `false`, whatever
    /// class it names.
    Raised {
        message: String,
        error_type: Option<String>,
        refused_the_field: bool,
    },
    /// A bound of this module stopped the run. The text says which.
    Stopped(String),
    /// The code defines nothing this call could run: it does not compile, or it does not
    /// export both of the functions a mapping is defined by.
    Faults(Vec<CodeFault>),
}

/// One whole evaluation: what the run came to, and everything else it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    /// What the run came to.
    pub outcome: Outcome,
    /// The log lines the run wrote, through `util.log` and `console`.
    pub logs: Vec<String>,
    /// The stash as it stands after the run, as JSON text, or `None` where the run left it
    /// empty.
    pub stash: Option<String>,
    /// The errors the run appended through `util.appendError`, as the JSON text of a list,
    /// or `None` where it appended none.
    pub out_errors: Option<String>,
    /// The `extensions.*` calls the run made, in the order it made them.
    pub extensions: Vec<ExtensionCall>,
}

/// Evaluate `function` of `code` over `context`, on a thread of its own.
///
/// The engine, the bounds and the whole run live inside that thread, so nothing of the
/// engine crosses onto the executor serving the request, and the thread is the sandbox's own
/// rather than one of the executor's ([`in_sandbox`], which also bounds how many engines this
/// process runs at once). The CALLER's wait is bounded separately ([`CALLER_WAIT_CEILING`]):
/// a run the engine has not come back from by then is given up on, so the request is answered
/// whatever the engine is doing, and the given-up run costs one sandbox slot until it ends.
pub async fn evaluate(code: String, function: String, context: Json) -> Evaluation {
    match in_sandbox(ENGINE_THREAD_STACK_BYTES, move || {
        run(&code, &function, &context)
    })
    .await
    {
        Entered::Answered(evaluation) => evaluation,
        Entered::GaveUp(reason) => stopped(reason),
    }
}

/// How many sandbox engines this process runs at ONE time.
///
/// The bound that makes the cost of this operation finite however many requests arrive: each
/// run takes a slot for the life of the thread that owns its engine, so the engines in flight
/// — and with them the memory they may each take ([`RUN_MEMORY_BYTES`]) and the threads they
/// run on — can never exceed this count, and a run that outlasts the caller's wait costs that
/// one slot and nothing else. Chosen well above the number of evaluations a caller can have
/// in flight in practice, so an ordinary request never queues behind another.
const MAX_SANDBOX_ENGINES: usize = 16;

/// The slots the sandbox's engines take, one per engine ([`MAX_SANDBOX_ENGINES`]).
static SANDBOX_SLOTS: ::tokio::sync::Semaphore =
    ::tokio::sync::Semaphore::const_new(MAX_SANDBOX_ENGINES);

/// The name the sandbox's own threads carry, so one is recognisable in a thread dump.
const SANDBOX_THREAD_NAME: &str = "appsync-js-sandbox";

/// How much stack one of those threads is given for an ENGINE's run.
///
/// The engine holds its own, much smaller ceiling ([`RUN_STACK_BYTES`]), which is what bounds
/// how deeply evaluated code may recurse, so the thread only has to carry the engine's own
/// work around it.
const ENGINE_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

/// What asking the sandbox to run one engine came to.
pub(crate) enum Entered<T> {
    /// The engine ran and this is what it produced.
    Answered(T),
    /// Nothing was learned: the host did not wait for it, or could not start it. The text
    /// says which.
    GaveUp(String),
}

/// Run `work` — one engine's whole life, or one READ of the code an engine would run
/// (`crate::state::code_diagnostics`) — on a thread of its own, under the sandbox's slot
/// bound, and answer what it produced or why the host stopped waiting for it.
///
/// `stack_bytes` is the stack that thread is given, which the CALLER sizes because what runs
/// on it decides: an engine's run is held to the engine's own stack ceiling
/// ([`ENGINE_THREAD_STACK_BYTES`]), while a read of caller-supplied code walks into that code
/// as deeply as it nests and sizes the thread for the deepest structure the code could be.
///
/// Two bounds, both of the HOST's and neither reachable from the evaluated code:
///
/// * the SLOT ([`SANDBOX_SLOTS`]), held by the thread that owns the engine and released when
///   that thread ends, so the engines in flight are bounded whatever the code does — a run
///   that swallowed everything the engine raised in it could cost at most its own slot, and
///   never a thread of the executor serving other requests;
/// * the caller's WAIT ([`CALLER_WAIT_CEILING`]), both for entering the sandbox and for the
///   answer, so the request is answered even where every slot is taken.
pub(crate) async fn in_sandbox<T: Send + 'static>(
    stack_bytes: usize,
    work: impl FnOnce() -> T + Send + 'static,
) -> Entered<T> {
    let Ok(Ok(slot)) =
        ::tokio::time::timeout(CALLER_WAIT_CEILING, SANDBOX_SLOTS.acquire()).await
    else {
        return Entered::GaveUp(format!(
            "the sandbox was busy for {} seconds",
            CALLER_WAIT_CEILING.as_secs()
        ));
    };
    let (answer, answered) = ::tokio::sync::oneshot::channel();
    let started = ::std::thread::Builder::new()
        .name(SANDBOX_THREAD_NAME.to_string())
        .stack_size(stack_bytes)
        .spawn(move || {
            let produced = work();
            // The engine is gone with `work`; the slot goes with the thread, so what the
            // next run finds free is a slot nothing holds any more.
            let _ = answer.send(produced);
            drop(slot);
        });
    if started.is_err() {
        return Entered::GaveUp("the sandbox could not be started".to_string());
    }
    match ::tokio::time::timeout(CALLER_WAIT_CEILING, answered).await {
        Ok(Ok(produced)) => Entered::Answered(produced),
        Ok(Err(_)) => Entered::GaveUp("the code could not be run".to_string()),
        Err(_) => Entered::GaveUp(format!(
            "the code was given up on after {} seconds",
            CALLER_WAIT_CEILING.as_secs()
        )),
    }
}

/// What a compile of a written definition came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compiled {
    /// The engine READ the code: the faults it found doing so, none for code it read to
    /// the end.
    Read(Vec<CodeFault>),
    /// The compile did not happen, or did not come back: the engine did not start, a bound
    /// of this module stopped it, or the host gave up waiting for it. The text says which.
    /// Nothing was learned about the code, which is a different answer from "it compiled".
    Unknown(String),
}

/// Compile `code` without running it, and report what the compile came to.
///
/// This is the check a call that WRITES a definition makes: a resolver or a function is
/// stored only if the engine that would RUN it can compile it, and nothing of the code
/// runs — no host function and no context is installed, and no module body is evaluated.
/// Every bound a run is held to holds here too (the engine's memory and stack ceilings,
/// the wall clock and the step budget, checked by the engine's own interrupt handler), so
/// one definition's nesting or size cannot take the host's stack: the engine raises inside
/// itself instead. The compile happens on a blocking thread, and the caller's wait is
/// bounded the way a run's is.
///
/// A compile that did not happen is reported as [`Compiled::Unknown`] rather than as a
/// clean one: the caller then answers the server fault its model declares instead of
/// storing a definition nothing could read.
pub async fn compile_faults(code: String) -> Compiled {
    match in_sandbox(ENGINE_THREAD_STACK_BYTES, move || compile(&code)).await {
        Entered::Answered(compiled) => compiled,
        Entered::GaveUp(reason) => Compiled::Unknown(reason),
    }
}

/// Compile `code` on THIS thread, under every bound this module declares, and report what
/// the compiler made of it.
fn compile(code: &str) -> Compiled {
    let Ok(runtime) = Runtime::new() else {
        return Compiled::Unknown("the engine did not start".to_string());
    };
    runtime.set_memory_limit(RUN_MEMORY_BYTES);
    // The engine's own stack ceiling, which is what makes a deeply nested definition a
    // bounded failure INSIDE the engine rather than a fault of the host's stack.
    runtime.set_max_stack_size(RUN_STACK_BYTES);
    // The engine resolves every `import` as it compiles, and what the compile asks is
    // whether the code can be READ: a specifier is therefore answered with an empty
    // module rather than with a host path or a resolution failure, so a definition
    // importing a helper module is not reported as code the engine could not read.
    // Nothing of that module is evaluated either — the compile never evaluates.
    runtime.set_loader(AnyResolver, EmptyLoader);
    let bounds = Arc::new(Bounds::new());
    let watched = Arc::clone(&bounds);
    runtime.set_interrupt_handler(Some(Box::new(move || watched.spent())));
    let Ok(engine_context) = Context::full(&runtime) else {
        return Compiled::Unknown("the engine did not start".to_string());
    };
    let faults = engine_context.with(|ctx| {
        match Module::declare(ctx.clone(), CODE_UNIT, code.to_string()).catch(&ctx) {
            Ok(_declared) => Vec::new(),
            Err(caught) => vec![compile_fault(&caught)],
        }
    });
    // A compile the clock or the step budget stopped did not read the code to the end, so
    // what it stopped at is not a fault OF the code: the breach is the answer, the way it
    // is for a run.
    match bounds.breach() {
        Some(breach) => Compiled::Unknown(breach),
        None => Compiled::Read(faults),
    }
}

/// How an `import` specifier is resolved for a COMPILE: to itself, whatever it names, so
/// no specifier fails to resolve and none is looked for on a host path.
#[derive(Debug, Clone, Copy)]
struct AnyResolver;

impl Resolver for AnyResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        _base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> ::rquickjs::Result<String> {
        Ok(name.to_string())
    }
}

/// How a resolved specifier is loaded for a COMPILE: as a module declared from this
/// module's own source. No path is opened and nothing is evaluated, so the import
/// contributes nothing but the fact that it resolved.
#[derive(Debug, Clone, Copy)]
struct EmptyLoader;

impl Loader for EmptyLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        path: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> ::rquickjs::Result<Module<'js>> {
        Module::declare(ctx.clone(), path.to_string(), EMPTY_MODULE_SOURCE)
    }
}

/// The source [`EmptyLoader`] declares every specifier from: a module with one default
/// member, so the import of a helper module resolves and the compile goes on reading the
/// code.
const EMPTY_MODULE_SOURCE: &str = "export default {};\n";

/// The fault code the engine could not compile takes: the engine's own account of where it
/// stopped reading, at the position it states.
///
/// The engine states the failure as one message. Where it carries a position of its own,
/// that is the fault's; where it carries none, the fault takes the values
/// `CodeErrorLocation` documents for an unknown one (`0`), and the caller of this module
/// is what decides whether the code itself states the position instead
/// (`crate::state::code_definition`).
fn compile_fault(caught: &::rquickjs::CaughtError<'_>) -> CodeFault {
    let (value, line, column) = match caught {
        ::rquickjs::CaughtError::Exception(exception) => {
            let stated = |member: &str| {
                exception
                    .get::<_, Option<i32>>(member)
                    .ok()
                    .flatten()
                    .unwrap_or_default()
            };
            (
                exception
                    .message()
                    .filter(|message| !message.is_empty())
                    .unwrap_or_else(|| exception.to_string()),
                stated("lineNumber"),
                stated("columnNumber"),
            )
        }
        other => (other.to_string(), 0, 0),
    };
    CodeFault {
        error_type: PARSE_ERROR,
        value,
        line,
        column,
        span: UNKNOWN_SPAN,
    }
}

/// An evaluation that produced nothing but the reason it was stopped.
fn stopped(reason: String) -> Evaluation {
    Evaluation {
        outcome: Outcome::Stopped(reason),
        logs: Vec::new(),
        stash: None,
        out_errors: None,
        extensions: Vec::new(),
    }
}

/// Evaluate `function` of `code` over `context` on THIS thread, under every bound this
/// module declares.
fn run(code: &str, function: &str, context: &Json) -> Evaluation {
    let collected = Arc::new(Mutex::new(Collected::default()));
    let Ok(context_json) = ::serde_json::to_string(context) else {
        return finished(
            Outcome::Stopped("the context could not be rendered".to_string()),
            &collected,
            None,
        );
    };
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            return finished(
                Outcome::Stopped(format!("the engine did not start: {error}")),
                &collected,
                None,
            )
        }
    };
    runtime.set_memory_limit(RUN_MEMORY_BYTES);
    runtime.set_max_stack_size(RUN_STACK_BYTES);
    runtime.set_loader(UtilsResolver, UtilsLoader);
    let bounds = Arc::new(Bounds::new());
    let watched = Arc::clone(&bounds);
    // The only thing that can stop code with no exit: a timeout cannot preempt a loop
    // running on this thread, but the engine asks this handler as it works.
    runtime.set_interrupt_handler(Some(Box::new(move || watched.spent())));
    let engine_context = match Context::full(&runtime) {
        Ok(engine_context) => engine_context,
        Err(error) => {
            return finished(
                Outcome::Stopped(format!("the engine did not start: {error}")),
                &collected,
                None,
            )
        }
    };
    let (outcome, stash) = engine_context.with(|ctx| {
        evaluate_in(&ctx, code, function, &context_json, &Arc::clone(&collected))
    });
    // A breached bound is the run's answer, whatever the code then said or returned: the
    // latch is host state the code cannot reach.
    match bounds.breach() {
        Some(breach) => finished(Outcome::Stopped(breach), &collected, stash),
        None => finished(outcome, &collected, stash),
    }
}

/// The whole evaluation: `outcome`, plus the lines and errors the run produced and the
/// stash it left.
fn finished(
    outcome: Outcome,
    collected: &Arc<Mutex<Collected>>,
    stash: Option<String>,
) -> Evaluation {
    let collected = collected.lock().unwrap_or_else(PoisonError::into_inner);
    Evaluation {
        outcome,
        logs: collected.logs.clone(),
        // An empty stash and an empty out-error list carry nothing, so the response states
        // neither: the members are there to report what the run left behind.
        stash: stash.filter(|text| !is_empty_object(text)),
        out_errors: collected.out_errors(),
        extensions: collected.extensions.clone(),
    }
}

/// Whether `text` is the JSON of an object with no members, which is a stash nothing wrote
/// to.
fn is_empty_object(text: &str) -> bool {
    text.trim() == "{}"
}

/// Compile the module, call `function` and read back what it returned and what the stash
/// holds, inside one engine context.
fn evaluate_in(
    ctx: &Ctx<'_>,
    code: &str,
    function: &str,
    context_json: &str,
    collected: &Arc<Mutex<Collected>>,
) -> (Outcome, Option<String>) {
    if let Err(error) = install_host(ctx, collected) {
        return (Outcome::Stopped(error), None);
    }
    if let Err(caught) = ctx.eval::<Value<'_>, _>(CONSOLE_SOURCE).catch(ctx) {
        return (
            Outcome::Stopped(format!("the sandbox console did not install: {caught}")),
            None,
        );
    }
    if let Err(error) = install_runtime_globals(ctx) {
        return (Outcome::Stopped(error), None);
    }
    // Held BEFORE any of the caller's code is compiled, so the invoker this run reads its
    // answer through is this module's own however the code rebinds the global object.
    let invoker = match ctx.eval::<Function<'_>, _>(INVOKER_SOURCE).catch(ctx) {
        Ok(invoker) => invoker,
        Err(caught) => {
            return (
                Outcome::Stopped(format!("the sandbox did not start: {caught}")),
                None,
            )
        }
    };
    let declared = match Module::declare(ctx.clone(), CODE_UNIT, code.to_string()).catch(ctx) {
        Ok(declared) => declared,
        // The engine resolves every `import` as it compiles, so a module this sandbox
        // serves none for is reported here beside code that does not parse.
        Err(caught) => return (Outcome::Faults(vec![parse_fault(&caught)]), None),
    };
    let (evaluated, settled) = match declared.eval().catch(ctx) {
        Ok(evaluated) => evaluated,
        Err(caught) => return (Outcome::Faults(vec![parse_fault(&caught)]), None),
    };
    // The module's body is evaluated through a promise, and nothing of this run drives a
    // job queue, so a body that RAISED is read off that promise here — otherwise its
    // failure would be lost and the hoisted exports would be called as though the module
    // had loaded.
    if let Err(caught) = settled.finish::<()>().catch(ctx) {
        if !pending(&caught) {
            return (
                Outcome::Raised {
                    message: raised_message(&caught),
                    error_type: None,
                    refused_the_field: false,
                },
                None,
            );
        }
    }
    // "For APPSYNC_JS runtimes, the code defines the request and response functions"
    // (`EvaluateCode`), so a module is a mapping only once it exports a function for BOTH
    // of them ([`EVALUATED_FUNCTIONS`]): each name it does not is reported as a fault of
    // the code and nothing is run, whichever of them the call asked to evaluate.
    let missing: Vec<CodeFault> = EVALUATED_FUNCTIONS
        .iter()
        .filter(|name| evaluated.get::<_, Function<'_>>(**name).is_err())
        .map(|name| missing_export_fault(name))
        .collect();
    if !missing.is_empty() {
        return (Outcome::Faults(missing), None);
    }
    let Ok(handler) = evaluated.get::<_, Function<'_>>(function) else {
        return (
            Outcome::Faults(vec![missing_export_fault(function)]),
            None,
        );
    };
    let argument = match ctx.json_parse(context_json.to_string()) {
        Ok(argument) => argument,
        Err(error) => {
            return (
                Outcome::Stopped(format!("the context could not be read: {error}")),
                None,
            )
        }
    };
    let answered = match invoker
        .call::<_, String>((handler, argument))
        .catch(ctx)
    {
        Ok(answered) => answered,
        // The invoker reports the code's own raise itself, so reaching here means the run
        // was interrupted out of it — which the latch has already recorded.
        Err(caught) => {
            return (
                Outcome::Raised {
                    message: raised_message(&caught),
                    error_type: None,
                    refused_the_field: false,
                },
                None,
            )
        }
    };
    read_answer(&answered)
}

/// The invoker's answer: what the run came to, and the stash it left.
fn read_answer(answered: &str) -> (Outcome, Option<String>) {
    let Ok(document) = ::serde_json::from_str::<Json>(answered) else {
        return (
            Outcome::Stopped("the sandbox answered with a document that is not JSON".to_string()),
            None,
        );
    };
    let member = |name: &str| {
        document
            .get(name)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let stash = document
        .get(STASH_MEMBER)
        .and_then(Json::as_str)
        .filter(|text| text.len() <= MAX_ANSWER_BYTES)
        .map(str::to_string);
    let outcome = match document.get("outcome").and_then(Json::as_str) {
        Some(reported @ ("returned" | RETURNED_EARLY_OUTCOME)) => {
            let result = member("result");
            if result.len() > MAX_ANSWER_BYTES {
                Outcome::Stopped(format!(
                    "the code returned more than {MAX_ANSWER_BYTES} bytes"
                ))
            } else if reported == RETURNED_EARLY_OUTCOME {
                Outcome::ReturnedEarly(result)
            } else {
                Outcome::Returned(result)
            }
        }
        Some("raised" | "unreadable") => Outcome::Raised {
            message: member("message"),
            // The `errorType` the raise named, which the invoker reports as the error's own
            // name; a raise that named none carries no type.
            error_type: document
                .get("errorType")
                .and_then(Json::as_str)
                .filter(|named| !named.is_empty())
                .map(str::to_string),
            // Whether the raise is `util.unauthorized`'s refusal of the field being resolved,
            // which the invoker reports from the marker that call raises with.
            refused_the_field: document
                .get("refusedTheField")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        },
        _ => Outcome::Stopped("the sandbox answered with no outcome".to_string()),
    };
    (outcome, stash)
}

/// What the sandbox's invoker ([`INVOKER_SOURCE`]) reports for a run the code ended through
/// `runtime.earlyReturn`, which is what tells that outcome apart from a plain return.
const RETURNED_EARLY_OUTCOME: &str = "returnedEarly";

/// The message a raise carries: the error's own, as the code worded it.
fn raised_message(caught: &::rquickjs::CaughtError<'_>) -> String {
    match caught {
        ::rquickjs::CaughtError::Exception(exception) => exception
            .message()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| exception.to_string()),
        other => other.to_string(),
    }
}

/// Whether `caught` is the engine saying the promise has not settled, which is not a
/// failure of the code: APPSYNC_JS code is synchronous, so a module that has not settled
/// after its body ran is one waiting on work nothing in this sandbox can do.
fn pending(caught: &::rquickjs::CaughtError<'_>) -> bool {
    matches!(
        caught,
        ::rquickjs::CaughtError::Error(::rquickjs::Error::WouldBlock)
    )
}

/// The fault code the engine could not READ takes.
///
/// The engine reports the failure as one message, which names where in the code it stopped;
/// it hands back no line or column of its own, so the location takes the values
/// `CodeErrorLocation` documents for an unknown one (0 for the line and the column,
/// [`UNKNOWN_SPAN`] for the span) and the message is reported verbatim as the user
/// presentable error. Where the fault is REPORTED decides whether the position is stated
/// from the code itself instead (`crate::state::stated_code_fault`).
fn parse_fault(caught: &::rquickjs::CaughtError<'_>) -> CodeFault {
    let value = match caught {
        ::rquickjs::CaughtError::Exception(exception) => exception
            .message()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| exception.to_string()),
        other => other.to_string(),
    };
    CodeFault {
        error_type: PARSE_ERROR,
        value,
        line: 0,
        column: 0,
        span: UNKNOWN_SPAN,
    }
}

/// The fault a module that does not export `name` as a function takes: the mapping the
/// runtime would have called is not there, stated as the export it looked for.
///
/// The module itself compiled, so the engine reports no position for it; the location takes
/// the values `CodeErrorLocation` documents for an unknown one, and the reader of the fault
/// states the position from the code (`crate::state::stated_code_fault`).
fn missing_export_fault(name: &str) -> CodeFault {
    CodeFault {
        error_type: MISSING_MAPPING_EXPORT,
        value: format!("The code does not export a {name} function."),
        line: 0,
        column: 0,
        span: UNKNOWN_SPAN,
    }
}

/// The lines and errors one run produced, under this module's ceilings.
#[derive(Debug, Default)]
struct Collected {
    /// The log lines, in the order the run wrote them.
    logs: Vec<String>,
    /// The appended errors, each as the JSON text of one error, in the order the run
    /// appended them.
    appended: Vec<String>,
    /// The `extensions.*` calls the run made, in the order it made them.
    extensions: Vec<ExtensionCall>,
}

impl Collected {
    /// Record one log line, under the line and length ceilings.
    fn log(&mut self, level: &str, message: &str) {
        if self.logs.len() >= MAX_LOG_ENTRIES {
            return;
        }
        let mut line = format!("{level} - {message}");
        if line.len() > MAX_LOG_ENTRY_BYTES {
            line.truncate(boundary_at_or_below(&line, MAX_LOG_ENTRY_BYTES));
        }
        self.logs.push(line);
    }

    /// Record one `extensions.*` call, under the count and length ceilings. A call whose
    /// argument is not a JSON document is dropped rather than recorded as one.
    fn extension(&mut self, name: &str, argument: &str) {
        if self.extensions.len() >= MAX_EXTENSION_CALLS
            || argument.len() > MAX_ANSWER_BYTES
            || ::serde_json::from_str::<Json>(argument).is_err()
        {
            return;
        }
        self.extensions.push(ExtensionCall {
            name: name.to_string(),
            argument: argument.to_string(),
        });
    }

    /// Record one appended error, under the count ceiling. An entry that is not a JSON
    /// document is dropped rather than put in a list the response says is JSON.
    fn append_error(&mut self, entry: &str) {
        if self.appended.len() >= MAX_OUT_ERRORS
            || entry.len() > MAX_ANSWER_BYTES
            || ::serde_json::from_str::<Json>(entry).is_err()
        {
            return;
        }
        self.appended.push(entry.to_string());
    }

    /// The appended errors as the JSON text of a list, or `None` where the run appended
    /// none. Assembled from the texts the engine rendered, so each error reads as the code
    /// wrote it.
    fn out_errors(&self) -> Option<String> {
        if self.appended.is_empty() {
            return None;
        }
        Some(format!("[{}]", self.appended.join(",")))
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

/// Install the host functions this sandbox carries: the log, the appended error, the id
/// mint, the DynamoDB marshalling and the record of the `extensions` calls the run made,
/// each of which only writes to or reads from THIS run.
///
/// Nothing else of the host is reachable from the engine.
fn install_host(ctx: &Ctx<'_>, collected: &Arc<Mutex<Collected>>) -> Result<(), String> {
    let logged = Arc::clone(collected);
    let appended = Arc::clone(collected);
    let extended = Arc::clone(collected);
    let globals = ctx.globals();
    globals
        .set(
            "__appsyncLog",
            Func::from(move |level: String, message: String| {
                logged
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .log(&level, &message);
            }),
        )
        .map_err(|error| format!("the sandbox log could not be installed: {error}"))?;
    globals
        .set(
            "__appsyncAppendError",
            Func::from(move |entry: String| {
                appended
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .append_error(&entry);
            }),
        )
        .map_err(|error| format!("the sandbox error list could not be installed: {error}"))?;
    globals
        .set(
            "__appsyncExtension",
            Func::from(move |name: String, argument: String| {
                extended
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extension(&name, &argument);
            }),
        )
        .map_err(|error| {
            format!("the sandbox extensions could not be installed: {error}")
        })?;
    globals
        .set("__appsyncAutoId", Func::from(auto_id))
        .map_err(|error| format!("the sandbox id mint could not be installed: {error}"))?;
    globals
        .set("__appsyncDynamoDb", Func::from(dynamodb_helper))
        .map_err(|error| {
            format!("the sandbox DynamoDB helpers could not be installed: {error}")
        })?;
    globals
        .set(RDS_HOST_FUNCTION, Func::from(rds_helper))
        .map_err(|error| {
            format!("the sandbox relational database helpers could not be installed: {error}")
        })
}

/// What one call of a `@aws-appsync/utils/rds` helper came to, as the JSON document the
/// sandbox reads back: `value` for what the helper answered, or `error` for the fault it
/// states.
///
/// The helpers themselves are the service's ONE reading of them
/// (`crate::service::graphql_api::relational_database::utilities`), so a statement this
/// runtime builds and the same statement built by a VELOCITY template carry one SQL text, and
/// a result both read reports one set of rows. Nothing of the host is reachable through it:
/// it is a pure transformation of the arguments the call was made with.
fn rds_helper(helper: String, arguments: String) -> String {
    use crate::service::graphql_api::relational_database::utilities;

    let args: Vec<::serde_json::Value> =
        ::serde_json::from_str(&arguments).unwrap_or_default();
    let answered = match utilities::call(&helper, &args) {
        Ok(Some(value)) => ::serde_json::json!({"value": value}),
        Ok(None) => ::serde_json::json!({"error": format!("{NO_SUCH_RDS_HELPER}{helper}")}),
        Err(fault) => ::serde_json::json!({"error": fault}),
    };
    answered.to_string()
}

/// How a call naming a relational-database helper this runtime does not publish is stated.
const NO_SUCH_RDS_HELPER: &str = "No such helper: ";

/// What one `util.dynamodb.<helper>` call came to, as the JSON document the sandbox reads
/// back: `value` for the representation it marshalled, or `error` for the fault it states.
///
/// The marshalling itself is the service's ONE reading of it
/// (`crate::service::graphql_api::mapping_template::utilities::dynamodb`), so a value this
/// runtime marshals and the same value marshalled by a VELOCITY template carry one
/// representation. Nothing of the host is reachable through it: it is a pure transformation
/// of the arguments the call was made with.
fn dynamodb_helper(helper: String, arguments: String) -> String {
    use crate::service::graphql_api::mapping_template::utilities::dynamodb;

    let args: Vec<::serde_json::Value> =
        ::serde_json::from_str(&arguments).unwrap_or_default();
    let answered = match dynamodb::call(&helper, &args) {
        Ok(Some(value)) => ::serde_json::json!({"value": value}),
        Ok(None) => ::serde_json::json!({"error": format!("{NO_SUCH_DYNAMODB_HELPER}{helper}")}),
        Err(fault) => ::serde_json::json!({"error": fault}),
    };
    answered.to_string()
}

/// How a call naming a `util.dynamodb` helper this runtime does not publish is stated.
const NO_SUCH_DYNAMODB_HELPER: &str = "No such helper: util.dynamodb.";

/// Bind the APPSYNC_JS runtime's own names in the GLOBAL scope: `util`, `runtime` and
/// `extensions`.
///
/// Code written for this runtime reaches them BOTH ways — through the import of the utilities
/// module ("`import { util } from '@aws-appsync/utils'`") and as bare names, because the
/// runtime binds them for the code it runs — so a handler that calls `util.error(...)` with
/// no import of its own is code this runtime runs rather than code that fails on an undefined
/// name.
///
/// They are the SAME objects the module exports ([`UTILS_MODULE_SOURCE`], declared here under
/// the module's own name so a later `import` of it resolves to this one instance): one
/// definition of each helper, so the two ways of reaching them cannot drift apart. Installed
/// before any of the caller's code is compiled, so code that rebinds a global only changes
/// what IT sees.
fn install_runtime_globals(ctx: &Ctx<'_>) -> Result<(), String> {
    let declared = Module::declare(ctx.clone(), UTILS_MODULE, UTILS_MODULE_SOURCE)
        .map_err(|error| format!("the sandbox utilities did not compile: {error}"))?;
    let (module, settled) = declared
        .eval()
        .map_err(|error| format!("the sandbox utilities did not load: {error}"))?;
    settled
        .finish::<()>()
        .map_err(|error| format!("the sandbox utilities did not load: {error}"))?;
    let globals = ctx.globals();
    for name in RUNTIME_GLOBALS {
        let exported: Value<'_> = module
            .get(*name)
            .map_err(|error| format!("the sandbox {name} is not available: {error}"))?;
        globals
            .set(*name, exported)
            .map_err(|error| format!("the sandbox {name} could not be installed: {error}"))?;
    }
    Ok(())
}

/// The names the runtime binds for the code it runs, which are the names
/// [`UTILS_MODULE_SOURCE`] exports.
const RUNTIME_GLOBALS: &[&str] = &["util", "runtime", "extensions"];

/// A fresh identifier for `util.autoId`, in the UUID form AppSync's own ids take.
///
/// Drawn from the clock and from a counter this process advances, both put through the
/// bijection this service already mints its opaque ids with (`crate::state`): the counter
/// is what tells two calls made in the same nanosecond apart, so no two calls — on this
/// thread or on another — answer with the same id.
fn auto_id() -> String {
    static TURNS: AtomicU64 = AtomicU64::new(0);
    let turn = TURNS.fetch_add(1, Ordering::Relaxed);
    let high = splitmix64(now_nanos() ^ turn);
    let low = splitmix64(high ^ turn.rotate_left(32));
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&high.to_be_bytes());
    bytes[8..].copy_from_slice(&low.to_be_bytes());
    ::uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// The invoker: the ONE thing that calls the evaluated function and renders what it came
/// to, as the JSON text of a document the host reads.
///
/// It is an expression rather than a global, so the evaluated code can neither read it nor
/// replace it, and it captures `JSON.stringify` and `String` before any of that code runs,
/// so a module that rebinds either cannot change how its own answer is rendered. The
/// result and the stash are handed back ALREADY rendered, so the response carries the
/// document the engine wrote rather than one re-serialised on the host.
const INVOKER_SOURCE: &str = r#"
(function () {
    const stringify = JSON.stringify;
    const toText = String;
    const render = function (value) {
        if (value === undefined) {
            return "null";
        }
        const text = stringify(value);
        return text === undefined ? "null" : text;
    };
    const described = function (error) {
        if (error === null || error === undefined) {
            return toText(error);
        }
        try {
            const message = error.message;
            if (message !== undefined && message !== null && toText(message).length > 0) {
                return toText(message);
            }
        } catch (ignored) { }
        try {
            return toText(error);
        } catch (ignored) {
            return "the code raised a value that cannot be read";
        }
    };
    // The `errorType` a raise named, which `util.error` writes onto the error's own name;
    // the engine's own name for a plain error names no type, so it reports none.
    const typed = function (error) {
        try {
            if (error === null || typeof error !== "object") {
                return null;
            }
            const name = error.name;
            if (name === undefined || name === null || toText(name) === "Error") {
                return null;
            }
            return toText(name);
        } catch (ignored) {
            return null;
        }
    };
    // The marker `util.unauthorized` raises with, which tells the REFUSAL of the field being
    // resolved apart from an error the code worded itself under the same class.
    const refusedTheField = function (error) {
        try {
            return error !== null && typeof error === "object"
                && error.__appsyncUnauthorized === true;
        } catch (ignored) {
            return false;
        }
    };
    const stashed = function (context) {
        try {
            if (context === null || typeof context !== "object") {
                return "{}";
            }
            return render(context.stash === undefined ? {} : context.stash);
        } catch (ignored) {
            return "{}";
        }
    };
    return function (handler, context) {
        let value;
        try {
            value = handler(context);
        } catch (error) {
            // `runtime.earlyReturn(obj)` ends the call with `obj` as its answer, so the
            // raise it unwinds with is the code RETURNING rather than failing — reported
            // under its own outcome, because a step whose request function returned this way
            // is over (nothing else of the step runs).
            if (error !== null && typeof error === "object" && error.__appsyncEarlyReturn === true) {
                return stringify({
                    outcome: "returnedEarly",
                    result: render(error.data),
                    stash: stashed(context),
                });
            }
            return stringify({
                outcome: "raised",
                message: described(error),
                errorType: typed(error),
                refusedTheField: refusedTheField(error),
                stash: stashed(context),
            });
        }
        try {
            return stringify({
                outcome: "returned",
                result: render(value),
                stash: stashed(context),
            });
        } catch (error) {
            return stringify({
                outcome: "unreadable",
                message: described(error),
                errorType: typed(error),
                refusedTheField: refusedTheField(error),
                stash: stashed(context),
            });
        }
    };
})()
"#;

/// The `util` helpers [`UTILS_MODULE`] exports to the evaluated code, as the module's own
/// source.
///
/// Each one is either the code's OWN control flow or a value this sandbox can answer with:
/// `util.error` and `util.unauthorized` RAISE, which is how a resolver refuses;
/// `util.appendError` "adds an error to the response" without ending the run, which is what
/// `outErrors` reports; `util.log.info` and `util.log.error` write the lines `logs` reports;
/// `util.autoId` mints an identifier; `util.dynamodb` MARSHALS a value as the attribute value
/// DynamoDB holds it as, through the service's one reading of that conversion
/// ([`dynamodb_helper`]), which is what a resolver over a DynamoDB data source writes its
/// request object with; `util.time` reads the clock through the engine's own `Date`; and
/// `extensions` STATES what the real-time plane is to do about a subscription or a
/// publication, which the host records for it.
/// Nothing else is offered, so code reaching for a helper this emulator does not implement
/// fails loudly rather than silently doing nothing.
const UTILS_MODULE_SOURCE: &str = r#"
const hostLog = __appsyncLog;
const hostAppendError = __appsyncAppendError;
const hostAutoId = __appsyncAutoId;
const hostDynamoDb = __appsyncDynamoDb;
const hostExtension = __appsyncExtension;
const rendered = function (value) {
    if (typeof value === "string") {
        return value;
    }
    const text = JSON.stringify(value);
    return text === undefined ? String(value) : text;
};
const joined = function (args) {
    return Array.prototype.map.call(args, rendered).join(" ");
};
const detail = function (message, errorType, data, errorInfo) {
    return {
        message: message === undefined || message === null ? "" : String(message),
        errorType: errorType === undefined || errorType === null ? null : String(errorType),
        data: data === undefined ? null : data,
        errorInfo: errorInfo === undefined ? null : errorInfo,
    };
};
const raise = function (message, errorType, data, errorInfo) {
    const described = detail(message, errorType, data, errorInfo);
    const error = new Error(described.message);
    if (described.errorType !== null) {
        error.name = described.errorType;
    }
    error.data = described.data;
    error.errorInfo = described.errorInfo;
    throw error;
};
// Each `util.dynamodb` helper is the host's own marshalling of the value it is handed, so
// this runtime and a Velocity template write one representation of a value. A helper that
// cannot convert what it was handed raises, as every other fault of a run does.
const marshaller = function (name) {
    return function () {
        const answered = JSON.parse(
            hostDynamoDb(name, JSON.stringify(Array.prototype.slice.call(arguments)))
        );
        if (answered.error !== undefined && answered.error !== null) {
            throw new Error(answered.error);
        }
        return answered.value;
    };
};
const dynamodb = { fromS3ObjectJson: marshaller("fromS3ObjectJson") };
[
    "toDynamoDB",
    "toString",
    "toNumber",
    "toBinary",
    "toBoolean",
    "toNull",
    "toList",
    "toMap",
    "toMapValues",
    "toStringSet",
    "toNumberSet",
    "toBinarySet",
    "toS3Object",
].forEach(function (name) {
    // Every converter has a sibling answering the same representation already serialized,
    // which is what a template or a resolver inlines into the request it is writing.
    dynamodb[name] = marshaller(name);
    dynamodb[name + "Json"] = marshaller(name + "Json");
});
export const util = {
    error: raise,
    dynamodb: dynamodb,
    appendError: function (message, errorType, data, errorInfo) {
        hostAppendError(JSON.stringify(detail(message, errorType, data, errorInfo)));
    },
    unauthorized: function () {
        const error = new Error("Unauthorized");
        error.name = "Unauthorized";
        // The marker that tells this REFUSAL of the field being resolved apart from an error
        // the code worded itself, which `util.error` may give the very same class: the host
        // that resolves a field words the refusal, because it alone knows the field.
        error.__appsyncUnauthorized = true;
        throw error;
    },
    autoId: function () {
        return hostAutoId();
    },
    log: {
        info: function () { hostLog("INFO", joined(arguments)); },
        error: function () { hostLog("ERROR", joined(arguments)); },
    },
    time: {
        nowISO8601: function () { return new Date().toISOString(); },
        nowEpochSeconds: function () { return Math.floor(Date.now() / 1000); },
        nowEpochMilliSeconds: function () { return Date.now(); },
    },
};
export const runtime = {
    earlyReturn: function (value) {
        const error = new Error("early return");
        error.name = "EarlyReturn";
        error.__appsyncEarlyReturn = true;
        error.data = value === undefined ? null : value;
        throw error;
    },
};
// Each `extensions` call STATES something the real-time plane does with the subscription or
// the publication it belongs to, so the call is recorded for the host that owns those: the
// filter a subscribe-time handler installs, the invalidation filter it installs beside it,
// and the invalidation a mutation's response handler raises. The call returns nothing, as
// each of them does.
const extensionCall = function (name) {
    return function (argument) {
        hostExtension(name, JSON.stringify(argument === undefined ? null : argument));
    };
};
export const extensions = {
    setSubscriptionFilter: extensionCall("setSubscriptionFilter"),
    setSubscriptionInvalidationFilter: extensionCall("setSubscriptionInvalidationFilter"),
    invalidateSubscriptions: extensionCall("invalidateSubscriptions"),
};
export default { util, runtime, extensions };
"#;

/// The `console` the evaluated code writes through, installed by the HOST before any of
/// that code is compiled and holding the log function it writes to at the moment it is
/// installed, so a module that rebinds the host function cannot redirect it.
///
/// `console.log` and `console.error` are what an APPSYNC_JS resolver logs with beside
/// `util.log`, and both land in the `logs` the response reports. Code that replaces
/// `globalThis.console` only stops its OWN lines from being logged.
const CONSOLE_SOURCE: &str = r#"
(function () {
    const hostLog = __appsyncLog;
    const rendered = function (value) {
        if (typeof value === "string") {
            return value;
        }
        const text = JSON.stringify(value);
        return text === undefined ? String(value) : text;
    };
    const writer = function (level) {
        return function () {
            hostLog(level, Array.prototype.map.call(arguments, rendered).join(" "));
        };
    };
    globalThis.console = {
        log: writer("INFO"),
        info: writer("INFO"),
        debug: writer("INFO"),
        warn: writer("ERROR"),
        error: writer("ERROR"),
    };
})()
"#;

/// The run's budget, as the engine's interrupt handler spends it, and the latch that says
/// which bound ended the run.
#[derive(Debug)]
struct Bounds {
    /// When the run must be over.
    deadline: Instant,
    /// How many engine checks are left.
    checks: AtomicU64,
    /// Set once the clock ran out.
    timed_out: AtomicBool,
    /// Set once the checks ran out.
    steps_spent: AtomicBool,
}

impl Bounds {
    /// A budget starting now.
    fn new() -> Self {
        Self {
            deadline: Instant::now() + RUN_WALL_CLOCK,
            checks: AtomicU64::new(RUN_ENGINE_CHECKS),
            timed_out: AtomicBool::new(false),
            steps_spent: AtomicBool::new(false),
        }
    }

    /// Whether the run is over, asked by the engine as it works: the clock, then the step
    /// budget. Each answer latches, so the breach survives code that catches the exception
    /// the engine raises.
    fn spent(&self) -> bool {
        if Instant::now() >= self.deadline {
            self.timed_out.store(true, Ordering::Relaxed);
            return true;
        }
        if self.checks.fetch_sub(1, Ordering::Relaxed) <= 1 {
            self.steps_spent.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// What the breached bound is reported as, or `None` when none was breached.
    fn breach(&self) -> Option<String> {
        if self.timed_out.load(Ordering::Relaxed) {
            return Some(format!(
                "the code was stopped after {} seconds",
                RUN_WALL_CLOCK.as_secs()
            ));
        }
        if self.steps_spent.load(Ordering::Relaxed) {
            return Some(format!(
                "the code was stopped after {RUN_ENGINE_CHECKS} engine steps"
            ));
        }
        None
    }
}

/// How an `import` specifier is resolved: to the modules this runtime PROVIDES
/// ([`PROVIDED_MODULES`]) and to nothing else, so no host path and no other module is
/// reachable from the evaluated code.
#[derive(Debug, Clone, Copy)]
struct UtilsResolver;

impl Resolver for UtilsResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> ::rquickjs::Result<String> {
        if PROVIDED_MODULES.contains(&name) {
            return Ok(name.to_string());
        }
        Err(::rquickjs::Error::new_resolving_message(
            base.to_string(),
            name.to_string(),
            format!(
                "the modules an APPSYNC_JS runtime provides are {}",
                PROVIDED_MODULES.join(", ")
            ),
        ))
    }
}

/// The modules this runtime provides, each by the specifier an `import` names it with, and
/// the source each is declared from ([`UtilsLoader`]).
const PROVIDED_MODULES: &[&str] = &[UTILS_MODULE, RDS_UTILS_MODULE];

/// How a provided module is loaded: from the source this crate carries for it. No path is
/// opened for either.
#[derive(Debug, Clone, Copy)]
struct UtilsLoader;

impl Loader for UtilsLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        path: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> ::rquickjs::Result<Module<'js>> {
        let source = if path == UTILS_MODULE {
            UTILS_MODULE_SOURCE
        } else if path == RDS_UTILS_MODULE {
            RDS_UTILS_MODULE_SOURCE
        } else {
            return Err(::rquickjs::Error::new_loading(path.to_string()));
        };
        Module::declare(ctx.clone(), path.to_string(), source)
    }
}
