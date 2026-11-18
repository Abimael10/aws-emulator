// SPDX-License-Identifier: CC0-1.0

//! RUNNING a notebook execution's notebook: what the execution's own work answered.
//!
//! One concern of this service's state, split out of [`super::notebook_executions`] the way
//! the cluster's step runner ([`super::step_runner`]) is split out of the step lifecycle:
//! the lifecycle owns the statuses and their spans, and this owns the one thing that needs
//! more than this service's state — whether the notebook the execution names RAN, and what
//! it came to.
//!
//! The notebook is a real document in a real bucket: "the Amazon S3 location that the
//! notebook is stored in" (`StartNotebookExecutionInput$NotebookS3Location` doc, Smithy
//! model), so it is FETCHED from Amazon S3 over the emulator's own in-process callback
//! ([`::service_common::s3_client`]) exactly as any emulated service follows a reference
//! into another, and read as the Jupyter notebook format: its code cells, in order, are the
//! program the execution submits ([`notebook_code_cells`]).
//!
//! Those cells are then EXECUTED, by the contained Python interpreter this emulator already
//! holds for running caller-supplied code
//! ([`::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python`]): one
//! interpreter per run, built with its host-reaching surface compiled OUT, its native
//! modules narrowed to an allow-list and every bound inside the run — deadline, traced
//! steps, memory, recursion depth, output size — enforced in host code the cells cannot
//! reach. The cells reach no other service of this emulator (the run is given no seam to
//! dispatch an AWS call on) and no host path, host environment variable or host credential:
//! the only environment they see is the execution's own `EnvironmentVariables`.
//!
//! Around the run, this module holds what one EXECUTION may cost: the notebook object is
//! bounded before and after it is read ([`MAX_NOTEBOOK_BYTES`]), so is the number of cells
//! ([`MAX_NOTEBOOK_CELLS`]), the runs this process performs at once are a bounded pool
//! ([`MAX_CONCURRENT_NOTEBOOK_RUNS`]) each holding its place until its thread ends, and a run
//! is waited for no longer than its own wall clock and the grace on top of it. Nothing of a
//! run happens on the async executor, nothing of it happens under this service's store lock,
//! and nothing of it happens on the `StartNotebookExecution` request path
//! ([`start_notebook_run`]): that call answers once the cluster has received the request, and
//! the run's answer reaches the execution's record when the run is done.
//!
//! The cells share one set of globals and run in order, which is what a notebook kernel does
//! ([`NOTEBOOK_KERNEL_SOURCE`]), and the execution's end is what that run answered:
//!
//! * every code cell ran without raising — the execution FINISHES, and the EXECUTED
//!   notebook (each code cell carrying what its own run produced) is stored at the output
//!   location the request named, which is what `OutputNotebookURI` then points at
//!   ([`super::store_output_notebook`]);
//! * a cell raised — the execution FAILS, and `LastStateChangeReason` is the exception the
//!   cells produced, which is the engine's own reason for the failure;
//! * the run outlasted one of its bounds — the execution FAILS for that bound;
//! * nothing could be run at all (the execution names no Amazon S3 notebook, Amazon S3
//!   holds no such object, the object is no readable notebook, or its kernel is not a
//!   Python one this emulator can run) — the execution keeps the lifecycle's own
//!   not-executed failure ([`super::NOTEBOOK_NOT_EXECUTED_REASON`]), which says no notebook
//!   was run rather than reporting an outcome for work nothing performed;
//! * the run never reported at all — the lifecycle ends the execution at the longest a run
//!   may take to answer ([`NOTEBOOK_RUN_CEILING`]), so an execution is never left `RUNNING`
//!   for good.

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python::{
    self, Budget, Completion, Finished, Stop,
};
use smithy_server_runtime::EmbeddedEmulator;

use super::*;
use crate::ops::types::{NotebookS3LocationFromInput, OutputNotebookS3LocationFromInput};

/// The KERNEL a notebook execution's cells run in: the program that compiles and executes
/// each cell, in order, against one set of globals.
///
/// That is what a notebook kernel does — a notebook is a sequence of cells sharing one
/// namespace, so a name a cell binds is a name the next cell sees — and it is all this
/// program does: the cells are the caller's own code, compiled and executed by the
/// interpreter, and the answer is how many of them ran. A cell that raises propagates out
/// of here, so the exception the run reports is the notebook's own.
const NOTEBOOK_KERNEL_SOURCE: &str = r#"
import sys


class _CellOutput:
    """What one cell writes to the standard output stream, kept as the cell's own output.

    A notebook records each cell's output on the cell, so the kernel has to know which cell
    wrote what: the stream is bound to this for the length of one cell and the text is
    passed through to the stream it replaced, so the run's own log still carries everything
    the notebook wrote.
    """

    def __init__(self, stream):
        self.stream = stream
        self.parts = []

    def write(self, text):
        self.parts.append(text)
        return self.stream.write(text)

    def flush(self):
        self.stream.flush()

    def isatty(self):
        return False

    def text(self):
        return "".join(self.parts)


def execute(event, context):
    cells = event.get("cells") or []
    namespace = {"__name__": "__main__"}
    outputs = []
    for index, source in enumerate(cells):
        captured = _CellOutput(sys.stdout)
        sys.stdout = captured
        try:
            exec(compile(source, "In[{}]".format(index + 1), "exec"), namespace)
        finally:
            sys.stdout = captured.stream
        outputs.append(captured.text())
    return {"cells": outputs}
"#;

/// What the kernel is HANDED and what it ANSWERS under: the cells it is to run, and the
/// output of each cell it ran, in the order it ran them.
const NOTEBOOK_KERNEL_ANSWER: &str = "cells";

/// The module the kernel is imported from, and the handler the runtime resolves in it.
const NOTEBOOK_KERNEL_MODULE: &str = "emr_notebook_kernel.py";
const NOTEBOOK_KERNEL_HANDLER: &str = "emr_notebook_kernel.execute";

/// The stack one notebook run is given, on the thread of its own it runs on.
///
/// CHOSEN: the interpreter is recursive over the cells' own syntax, so a run needs more
/// stack than a request thread carries; it is the same order as the stack this emulator
/// gives any other run of caller-supplied code.
const RUN_STACK_BYTES: usize = 16 * 1024 * 1024;

/// How long the cells of one notebook may run for.
///
/// CHOSEN, not documented: a real execution runs for as long as its notebook does, on a
/// cluster. Here it runs off the request path, so the bound is what one execution of this
/// emulator may spend on a notebook — and a notebook that outlasts it is reported as having
/// failed on this bound rather than left running.
///
/// It is the bound the INTERPRETER the cells run in is built under as well, not only the
/// cells' own work, and several runs of this process are performed at once
/// ([`MAX_CONCURRENT_NOTEBOOK_RUNS`]) — so it has to leave room for that construction on a
/// host doing all of them at once, or a notebook of one cell that computes would be reported
/// as having outlasted a bound it never reached. Generous for that reason: it is a ceiling on
/// a runaway notebook, never a budget a notebook that runs has to fit in.
const NOTEBOOK_WALL_CLOCK: Duration = Duration::from_secs(15);

/// How much longer than its own wall clock a run is waited for before it is given up on.
///
/// CHOSEN: a run stops at the next event its interpreter delivers after the deadline, and
/// the interpreter is then finalized — the guest's own exit callbacks run under the same
/// bounds — so the answer of a run that was stopped arrives a little after its wall clock
/// rather than at it. The grace covers that, so a notebook stopped by its own bounds is
/// reported as stopped rather than as one nothing could be learned about.
const NOTEBOOK_RUN_GRACE: Duration = Duration::from_secs(5);

/// How many notebook runs this process performs AT ONCE.
///
/// CHOSEN: a run is an interpreter, a thread and the memory the two hold, so the number of
/// them this process carries is bounded rather than following from how many executions a
/// caller starts. The permit is held by the run's own thread and released when that thread
/// ends, so a run that was given up on still counts against the pool for as long as it
/// lasts.
const MAX_CONCURRENT_NOTEBOOK_RUNS: usize = 4;

/// How long a run waits for a place in the pool above to come free before nothing is run for
/// it.
///
/// CHOSEN: nothing is waiting on the answer — `StartNotebookExecution` has already answered,
/// and the execution whose run this is sits at `RUNNING`, "being processed by the cluster",
/// while it queues — so the wait covers TWO full-length runs ahead of it, each spending its
/// own wall clock and the grace on top of it, which leaves a queued run headroom when the
/// pool is busy rather than reporting a notebook that would have run as one nothing ran. It
/// stays bounded ([`NOTEBOOK_RUN_CEILING`] is what the wait costs the execution): past it,
/// the execution is reported as one nothing ran rather than left queued.
const NOTEBOOK_PERMIT_WAIT: Duration = NOTEBOOK_WALL_CLOCK
    .saturating_add(NOTEBOOK_RUN_GRACE)
    .saturating_mul(2);

/// The longest a run may take to REPORT what the notebook came to, counted from the moment
/// the execution reached `RUNNING`.
///
/// DERIVED, not chosen: every part of it is a bound this module already holds — a run waits
/// for a place in the pool ([`NOTEBOOK_PERMIT_WAIT`]) and then spends at most its own wall
/// clock and the grace on top of it — so a run that is going to answer at all has answered
/// by then, and nothing that WOULD have answered is cut short by this. It is what the
/// lifecycle owes an execution still `RUNNING`
/// ([`NotebookExecutionRecord::advance_to`]): past it the execution ends for having no
/// outcome rather than staying `RUNNING` for good, so a client polling one always reaches a
/// status it can act on.
pub const NOTEBOOK_RUN_CEILING: Duration = NOTEBOOK_PERMIT_WAIT
    .saturating_add(NOTEBOOK_WALL_CLOCK)
    .saturating_add(NOTEBOOK_RUN_GRACE);

/// The runs this process may perform at once ([`MAX_CONCURRENT_NOTEBOOK_RUNS`]).
fn notebook_run_permits() -> &'static ::std::sync::Arc<::tokio::sync::Semaphore> {
    static PERMITS: ::std::sync::OnceLock<::std::sync::Arc<::tokio::sync::Semaphore>> =
        ::std::sync::OnceLock::new();
    PERMITS.get_or_init(|| {
        ::std::sync::Arc::new(::tokio::sync::Semaphore::new(MAX_CONCURRENT_NOTEBOOK_RUNS))
    })
}

/// What one notebook run may spend, beside its wall clock.
///
/// CHOSEN, every one of them: a notebook is caller-supplied code, so each bound is what one
/// execution of this emulator may cost — traced events, the memory the run may add to this
/// process, the output kept, the answer the kernel may return and how deep the cells may
/// recurse. They are enforced by the interpreter's own host-side bounds, which the cells
/// cannot reach.
fn notebook_budget() -> Budget {
    Budget {
        wall_clock: NOTEBOOK_WALL_CLOCK,
        steps: 2_000_000,
        memory_bytes: 256 * 1024 * 1024,
        log_bytes: 16 * 1024,
        response_bytes: 64 * 1024,
        recursion_limit: 2_000,
    }
}

/// How many bytes of a notebook object are read at most.
///
/// CHOSEN: a notebook is a document a person edits, so this is well above any real one and
/// bounds what one request makes this process hold. It is taken TWICE: against the length
/// Amazon S3 reports, before any of the object is held, and against the bytes that actually
/// arrived, so the cap holds whatever the answer claimed.
pub const MAX_NOTEBOOK_BYTES: usize = 16 * 1024 * 1024;

/// How many code cells of one notebook are run.
///
/// CHOSEN, for the same reason the notebook's own size is bounded: the cells are the
/// program one request submits.
pub const MAX_NOTEBOOK_CELLS: usize = 1_000;

/// The notebook DOCUMENT `document` holds, or `None` when it is no notebook this emulator
/// can run.
///
/// The Jupyter notebook format holds its cells in a top-level `cells` array, each with a
/// `cell_type` and a `source` that is either one string or the list of its lines. A document
/// with no `cells` array is no notebook, and one whose kernel is not a Python kernel is a
/// notebook this emulator cannot run: the interpreter it runs cells in is a Python one, so a
/// Scala or R notebook is reported as not having been run rather than run as something it is
/// not. A notebook that states no kernel at all is taken as the Python kind, which is what
/// an Amazon EMR notebook's default kernel is.
///
/// The document is kept whole, rather than only its cells' sources: it is what the EXECUTED
/// notebook is built from once the cells have run ([`super::executed_notebook`]).
pub fn notebook_document(document: &[u8]) -> Option<::serde_json::Value> {
    let notebook: ::serde_json::Value = ::serde_json::from_slice(document).ok()?;
    if !notebook_is_python(&notebook) {
        return None;
    }
    notebook.get("cells")?.as_array()?;
    Some(notebook)
}

/// The code cells of `notebook`, in the order the notebook holds them: the program the
/// execution submits.
///
/// A cell that submits no program carries none of it ([`notebook_cell_program`]).
pub fn notebook_code_cells(notebook: &::serde_json::Value) -> Vec<String> {
    notebook
        .get("cells")
        .and_then(::serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(notebook_cell_program)
        .take(MAX_NOTEBOOK_CELLS)
        .collect()
}

/// Whether `notebook`'s kernel is a Python one — the kind whose cells this emulator can
/// run.
///
/// The notebook format states its language in `metadata.kernelspec.language` and in
/// `metadata.language_info.name`; either saying Python is enough, and a notebook that states
/// neither is taken as the Python kind (see [`notebook_document`]).
fn notebook_is_python(notebook: &::serde_json::Value) -> bool {
    let metadata = notebook.get("metadata");
    let stated = |path: [&str; 2]| -> Option<String> {
        Some(
            metadata?
                .get(path[0])?
                .get(path[1])?
                .as_str()?
                .to_lowercase(),
        )
    };
    let languages = [
        stated(["kernelspec", "language"]),
        stated(["kernelspec", "name"]),
        stated(["language_info", "name"]),
    ];
    let stated: Vec<String> = languages.into_iter().flatten().collect();
    stated.is_empty() || stated.iter().any(|language| language.contains("python"))
}

/// The PROGRAM one cell of a notebook submits, or `None` for a cell that submits none: a
/// cell whose type is not `code` (a Markdown cell is prose) and a cell whose `source` is
/// neither spelling the notebook format allows.
///
/// The one reading of what the kernel is handed, so the cells whose outputs are recorded on
/// the EXECUTED notebook ([`super::executed_notebook`]) are exactly the cells that ran.
pub fn notebook_cell_program(cell: &::serde_json::Value) -> Option<String> {
    if cell
        .get("cell_type")
        .and_then(::serde_json::Value::as_str)
        .is_none_or(|kind| kind != "code")
    {
        return None;
    }
    cell_source(cell)
}

/// One cell's `source`, as the notebook format spells it: one string, or the list of the
/// cell's lines.
fn cell_source(cell: &::serde_json::Value) -> Option<String> {
    notebook_text(cell.get("source"))
}

/// The TEXT a member of a notebook holds, as the notebook format spells it: one string, or
/// the list of its lines.
///
/// The ONE reading of that spelling in this crate: a cell's `source` is spelled this way and
/// so is a stream output's `text` on the EXECUTED notebook
/// ([`super::executed_notebook`]), so neither can come to read it differently.
pub fn notebook_text(value: Option<&::serde_json::Value>) -> Option<String> {
    match value? {
        ::serde_json::Value::String(text) => Some(text.clone()),
        ::serde_json::Value::Array(lines) => Some(
            lines
                .iter()
                .filter_map(::serde_json::Value::as_str)
                .collect::<Vec<&str>>()
                .concat(),
        ),
        _ => None,
    }
}

/// Start this execution's OWN run of its notebook, off the request path, and record what it
/// answered.
///
/// `StartNotebookExecution` answers once the cluster has received the request —
/// "`START_PENDING` indicates that the cluster has received the execution request but
/// execution has not begun" (`NotebookExecution$Status` doc) — so the notebook is run AFTER
/// that answer rather than inside it: this hands the run to the emulator's own runtime and
/// returns at once. The run then writes what it came to into the execution's record, which
/// is the end of its status walk ([`NotebookExecutionRecord::record_outcome`]), so the
/// execution leaves `RUNNING` when its notebook is really done.
///
/// What one run may cost is unchanged ([`run_notebook_cells`]): the pool bounds how many of
/// them this process performs at once, however many executions a caller starts, and the
/// number of executions is itself bounded ([`MAX_NOTEBOOK_EXECUTIONS`]).
pub fn start_notebook_run<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &::std::sync::Arc<Eng>,
    region: &str,
    execution_id: String,
    notebook: NotebookRunRequest,
) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    // The emulator's own in-process transport, as the request reached this service with: the
    // run reads the notebook over it after the request has been answered.
    let callback = ctx.callback().clone();
    let region = region.to_string();
    let run_state = ::engine::ServiceState::clone(&state);
    let run_id = execution_id.clone();
    // The run's own handle is recorded under the guard the run itself needs in order to
    // report, so a run that answers at once cannot clear its entry before this records it —
    // and the run stays abandonable for as long as it lasts.
    let mut store = state.lock();
    let handle = runtime.spawn(async move {
        let outcome = run_notebook(&callback, &region, &run_id, &notebook).await;
        let now = SystemTime::now();
        let mut store = run_state.lock();
        if let Some(record) = store.notebook_execution_mut(&run_id, now) {
            record.record_outcome(outcome, now);
        }
        // This run has answered, so there is nothing left to abandon.
        store.forget_notebook_run(&run_id);
    });
    store.set_notebook_run(execution_id, handle);
}

/// Everything OF THE REQUEST that reaches a run: the notebook to fetch, where its output
/// notebook is stored and in which format, and the environment the cells see.
///
/// Nothing else a caller sent steers any part of a run — not the engine it names, not its
/// tags, not its parameters — which is why the run is handed this and not the request.
#[derive(Debug, Clone, Default)]
pub struct NotebookRunRequest {
    /// "The Amazon S3 location that the notebook execution input is stored in"
    /// (`StartNotebookExecutionInput$NotebookS3Location` doc): the object to read.
    pub location: Option<NotebookS3LocationFromInput>,
    /// "The Amazon S3 location for the notebook execution output"
    /// (`StartNotebookExecutionInput$OutputNotebookS3Location` doc): where the executed
    /// notebook is stored ([`super::store_output_notebook`]).
    pub output_location: Option<OutputNotebookS3LocationFromInput>,
    /// "The output format for the notebook execution"
    /// (`StartNotebookExecutionInput$OutputNotebookFormat` doc).
    pub output_format: Option<String>,
    /// "The environment variables associated with the notebook execution"
    /// (`StartNotebookExecutionInput$EnvironmentVariables` doc): what the cells see, and the
    /// only environment they see.
    pub environment: Option<HashMap<String, String>>,
}

impl ComAmazonawsEmrElasticmapreduceState {
    /// Keep the task performing the execution `id`'s run, so the run can be abandoned while
    /// it lasts. A run this replaces is abandoned rather than merely forgotten.
    fn set_notebook_run(&mut self, id: String, handle: ::tokio::task::JoinHandle<()>) {
        if let Some(previous) = self.notebook_runs.insert(id, handle) {
            previous.abort();
        }
    }

    /// Forget the run of the execution `id` without abandoning it: what a run that has
    /// already answered calls.
    fn forget_notebook_run(&mut self, id: &str) {
        self.notebook_runs.remove(id);
    }

    /// ABANDON the run of the execution `id`, if one is still under way.
    ///
    /// What `StopNotebookExecution` asks for: "`STOP_PENDING` indicates that the cluster has
    /// received a `StopNotebookExecution` request and the stop is pending"
    /// (`NotebookExecution$Status` doc), and a stopped execution ends `STOPPED` rather than
    /// in what its notebook was going to come to — so the run it was waiting for is given up
    /// on rather than left to report an end nobody is waiting for.
    pub fn abandon_notebook_run(&mut self, id: &str) {
        if let Some(handle) = self.notebook_runs.remove(id) {
            handle.abort();
        }
    }
}

/// What the notebook execution `execution_id`'s own run answered, for the request's
/// `notebook`.
///
/// Called with NO store guard held: it follows the notebook's Amazon S3 reference over the
/// emulator's own in-process transport, runs the notebook's cells, and stores the EXECUTED
/// notebook back into Amazon S3 for a run whose cells all ran — none of which may happen
/// under this service's lock. Its one argument of the request is
/// [`NotebookRunRequest`], so nothing else a caller sent can steer any part of a run.
pub async fn run_notebook(
    callback: &EmbeddedEmulator,
    region: &str,
    execution_id: &str,
    notebook: &NotebookRunRequest,
) -> NotebookOutcome {
    // "The Amazon S3 location that the notebook execution input is stored in"
    // (`StartNotebookExecutionInput$NotebookS3Location` doc): an execution that names no
    // such location names a notebook under an editor this emulator holds no notebook
    // storage for, so there is nothing to fetch and nothing is reported as having run.
    let Some(location) = notebook.location.as_ref() else {
        return NotebookOutcome::NotRun;
    };
    let Some(object) = notebook_object(callback, region, location).await else {
        return NotebookOutcome::NotRun;
    };
    let Some(document) = notebook_document(&object) else {
        return NotebookOutcome::NotRun;
    };
    let cells = notebook_code_cells(&document);
    let environment: BTreeMap<String, String> = notebook
        .environment
        .iter()
        .flatten()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    match run_notebook_cells(cells, environment).await {
        // Every cell ran, so there is an EXECUTED notebook to store — the document the run
        // produced, at the location the request named ([`super::store_output_notebook`]).
        // `OutputNotebookURI` names what was really stored, and nothing at all when there
        // was no output location to store it at.
        CellRun::Finished(outputs) => NotebookOutcome::Finished {
            cells: outputs.len(),
            output_notebook_uri: store_output_notebook(
                callback,
                region,
                notebook.output_location.as_ref(),
                notebook.output_format.as_deref(),
                execution_id,
                &document,
                &outputs,
            )
            .await,
        },
        CellRun::Failed(reason) => NotebookOutcome::Failed { reason },
        CellRun::NotRun => NotebookOutcome::NotRun,
    }
}

/// The notebook object the Amazon S3 location `location` names, or `None` when Amazon S3
/// hands over no such object.
///
/// The read reaches nothing outside this process. The client is built from the operation's
/// own callback ([`::service_common::s3_client`]), which is the emulator's in-process
/// transport: no socket is opened, no name is resolved, and the ONE service it can reach is
/// this emulator's own Amazon S3. The location is a bucket and a key — an address inside
/// that store — so no part of the request becomes a host, a scheme, a URL or a filesystem
/// path, and an empty half of it addresses no object at all. The object is bounded by
/// [`MAX_NOTEBOOK_BYTES`] both on the length Amazon S3 reports, before any of it is held,
/// and on the bytes that arrived.
async fn notebook_object(
    callback: &EmbeddedEmulator,
    region: &str,
    location: &NotebookS3LocationFromInput,
) -> Option<Vec<u8>> {
    let bucket = location.bucket.as_deref().filter(|name| !name.is_empty())?;
    let key = location.key.as_deref().filter(|key| !key.is_empty())?;
    let read = ::service_common::s3_client::s3_client(callback, region)
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .ok()?;
    let over_the_cap = |length: usize| length > MAX_NOTEBOOK_BYTES;
    if read
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .is_some_and(over_the_cap)
    {
        return None;
    }
    let document = read.body.collect().await.ok()?.to_vec();
    (!over_the_cap(document.len())).then_some(document)
}

/// Run `cells` in the contained interpreter and report what they came to.
///
/// Three bounds hold this, beside the ones the interpreter enforces inside the run
/// ([`notebook_budget`]):
///
/// * a RUN of the process's pool ([`MAX_CONCURRENT_NOTEBOOK_RUNS`]), waited for no longer
///   than [`NOTEBOOK_PERMIT_WAIT`] — a run that finds the pool full queues for a place rather
///   than adding a thread to the process, and one that never gets a place is reported as
///   having run nothing. The permit is MOVED into the run's own thread, so it comes back when
///   that thread ends and a run that was given up on still holds its place in the pool while
///   it lasts;
/// * the run happens on a thread of its own, with the stack a run is given
///   ([`RUN_STACK_BYTES`]), because the interpreter is synchronous and CPU-bound: nothing of
///   it runs on the async executor, so the emulator stays responsive while it runs;
/// * this call waits for the answer no longer than the run's own wall clock and the grace on
///   top of it ([`NOTEBOOK_RUN_GRACE`]) — past that the run has been given up on and nothing
///   is reported as having come of it.
async fn run_notebook_cells(
    cells: Vec<String>,
    environment: BTreeMap<String, String>,
) -> CellRun {
    let budget = notebook_budget();
    let mut submitted = ::serde_json::Map::new();
    submitted.insert(NOTEBOOK_KERNEL_ANSWER.to_string(), ::serde_json::json!(cells));
    let Ok(event) = ::serde_json::to_string(&submitted) else {
        return CellRun::NotRun;
    };
    let Ok(Ok(permit)) = ::tokio::time::timeout(
        NOTEBOOK_PERMIT_WAIT,
        ::std::sync::Arc::clone(notebook_run_permits()).acquire_owned(),
    )
    .await
    else {
        ::tracing::warn!(
            runs = MAX_CONCURRENT_NOTEBOOK_RUNS,
            "no run came free inside the wait for a place in the pool, so the notebook was \
             not run"
        );
        return CellRun::NotRun;
    };
    let (sender, receiver) = ::tokio::sync::oneshot::channel();
    let spawned = ::std::thread::Builder::new()
        .name("emr-notebook-execution".to_string())
        .stack_size(RUN_STACK_BYTES)
        .spawn(move || {
            // Held for the life of this thread, so the pool counts the run until the thread
            // is really gone — including a run the caller below stopped waiting for.
            let _permit = permit;
            let modules = vec![PackageModule {
                path: NOTEBOOK_KERNEL_MODULE.to_string(),
                source: NOTEBOOK_KERNEL_SOURCE.to_string(),
            }];
            let finished = python::run(&python::Run {
                modules: &modules,
                handler: NOTEBOOK_KERNEL_HANDLER,
                environment: &environment,
                // The instruction set the environment the notebook runs in has, which the
                // run reports as its own machine. An execution names none, so it is the one
                // the engine's own runtime reports when none is named.
                architecture:
                    ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                event: Some(&event),
                request_id: "",
                invoked_function_arn: "",
                client_context: None,
                tenant_id: None,
                budget,
                // The cells reach no other service of this emulator: a notebook
                // execution is given no seam to dispatch an AWS call on.
                aws: None,
                // It is no Lambda durable execution's run either, so it takes no step of one
                // and checkpoints nothing onto one.
                durable: None,
            });
            let _ = sender.send(finished);
        });
    if spawned.is_err() {
        return CellRun::NotRun;
    }
    let waited = ::tokio::time::timeout(
        budget.wall_clock.saturating_add(NOTEBOOK_RUN_GRACE),
        receiver,
    )
    .await;
    match waited {
        Ok(Ok(finished)) => cell_run(&finished),
        // The run was given up on, or its thread ended without reporting: nothing is known
        // about what the notebook came to, so nothing is reported as having run.
        Ok(Err(_)) | Err(_) => CellRun::NotRun,
    }
}

/// What RUNNING a notebook's cells came to: the lifecycle's own end
/// ([`NotebookOutcome`]) follows from it, once the output notebook a finished run produced
/// has been stored.
enum CellRun {
    /// Every code cell ran without raising, and this is what each of them wrote to the
    /// standard output stream, in the order they ran.
    Finished(Vec<String>),
    /// The run ended in a failure, with the reason the engine gave for it.
    Failed(String),
    /// Nothing ran, so nothing is known about what the notebook came to.
    NotRun,
}

/// What the finished run answered.
fn cell_run(finished: &Finished) -> CellRun {
    match &finished.completion {
        // The kernel returns what each cell it ran wrote, which is the answer this reads: a
        // run that returned ran every cell of the notebook without raising.
        Completion::Returned(answer) => CellRun::Finished(cell_outputs(answer)),
        // The notebook's own exception, as the engine raised it: the type it is and the
        // message it carries, which is the reason the execution failed.
        Completion::Raised {
            error_type,
            error_message,
            ..
        } => CellRun::Failed(notebook_failure_reason(error_type, error_message)),
        // A bound the run ran past, named by the bound it was.
        Completion::Stopped(stop) => CellRun::Failed(notebook_bound_reason(*stop)),
        // The interpreter could not be built or driven, so no cell of the notebook ran.
        Completion::Internal(_) | Completion::Streamed { .. } => CellRun::NotRun,
    }
}

/// The output of each cell the kernel ran, read off the answer it returned.
fn cell_outputs(answer: &str) -> Vec<String> {
    ::serde_json::from_str::<::serde_json::Value>(answer)
        .ok()
        .as_ref()
        .and_then(|document| document.get(NOTEBOOK_KERNEL_ANSWER))
        .and_then(::serde_json::Value::as_array)
        .map(|outputs| {
            outputs
                .iter()
                .map(|output| {
                    output
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `NotebookExecution$LastStateChangeReason` for a notebook whose own cells raised: the
/// exception as the engine reports it.
///
/// Both parts come from the RUN — the exception's type and the message it carries — so the
/// reason a caller reads is the notebook's own failure and nothing this emulator worded.
fn notebook_failure_reason(error_type: &str, error_message: &str) -> String {
    if error_message.trim().is_empty() {
        return error_type.to_string();
    }
    format!("{error_type}: {error_message}")
}

/// `NotebookExecution$LastStateChangeReason` for a notebook the run STOPPED at one of its
/// bounds: which bound it was, in the notebook's own terms.
///
/// The bound is the run's, so the sentence states it as something the notebook did rather
/// than as something the notebook's code said: there is no exception to report, because the
/// cells were stopped rather than raised.
fn notebook_bound_reason(stop: Stop) -> String {
    let bound = match stop {
        Stop::Deadline => "ran longer than an execution may run for",
        Stop::Steps => "did more work than an execution may do",
        Stop::Memory => "needed more memory than an execution may use",
        Stop::Response => "produced more output than an execution may produce",
        Stop::HostCeiling => "took more of this emulator's host time than an execution may take",
    };
    format!("The notebook was stopped: it {bound}.")
}
