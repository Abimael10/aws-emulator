// SPDX-License-Identifier: CC0-1.0

//! The INFERENCE CODE a model artifact carries, and the inference that code computes.
//!
//! # The rule this module implements
//!
//! A SageMaker model is a container image plus "the S3 path where the model artifacts … are
//! stored" (`ContainerDefinition$ModelDataUrl`), and for the framework inference containers the
//! artifact carries the model's own INFERENCE CODE beside its parameters: the handler script in
//! the artifact's `code` directory, whose entry point the container reads out of its
//! `SAGEMAKER_PROGRAM` environment variable ("the environment variables to set in the Docker
//! container", `ContainerDefinition$Environment`) and which is `inference.py` by convention. The
//! container calls that script's own functions for every request it serves — `model_fn` loads the
//! model out of the model directory, `input_fn` reads the payload, `predict_fn` computes the
//! prediction and `output_fn` renders the answer, with the single `transform_fn` replacing the
//! last three — so what an invocation of such a model answers is whatever that code returned.
//!
//! So that is what happens here: an artifact carrying such a script is served by RUNNING it, over
//! the caller's own payload, and the bytes the script answered are the bytes the invocation (or
//! the batch transform's output object) carries. Nothing is synthesised and nothing is read off
//! the script's source text: a script that raises fails the request with its own exception, and
//! one that answers answers.
//!
//! # The engine is NOT this module's
//!
//! No execution substrate is introduced here. The interpreter is
//! [`::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python`] — the ONE
//! contained RustPython this workspace embeds for running caller-supplied Python, driven exactly
//! as the AWS Systems Manager automation plane and the Amazon Bedrock code interpreter drive it.
//! Everything that CONTAINS a run therefore lives in that one module and is maintained once: its
//! native modules are an allow-list with no socket, subprocess, memory-map, native-library or
//! thread module in it, its operating-system modules are that crate's own in-memory ones with
//! every host-reaching call refused, every way of opening a file of the HOST is taken away before
//! a line of guest code runs, and the deadline, step, memory, recursion and answer bounds are
//! enforced in host code the script cannot reach — including the latch that makes a stopped run's
//! answer that stop whatever the script catches.
//!
//! What this module adds is the inference container's own wiring, and nothing else:
//!
//! * the KERNEL ([`INFERENCE_KERNEL_SOURCE`]) — ordinary Python, handed to the interpreter as a
//!   module of the run the way every other module this workspace supplies one is: it imports the
//!   artifact's own entry point and calls the handler functions above in the order the container
//!   calls them;
//! * the MODEL DIRECTORY the script reads its parameters out of: the artifact's other entries,
//!   handed to the run as an in-memory, READ-ONLY mapping under [`super::MODEL_DIRECTORY`], so a
//!   script that opens its model file reads the artifact that was deployed and no path of the
//!   host resolves at all;
//! * the host-side bounds around the run: it happens on a thread of its own with the stack a run
//!   of caller-supplied code is given, under this process's own count of concurrent script runs,
//!   and the caller waits for its answer no longer than the host time one run may take and a
//!   grace on top of it — past that the run has been given up on, its thread keeps its slot until
//!   it really ends, and the request is answered with that condition rather than left waiting.

use super::*;

use ::std::sync::{Condvar, Mutex};
use ::std::time::Duration;

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::INIT_PHASE_LIMIT;

/// The directory of a model artifact that holds the model's own inference code: "you can include
/// your inference script in the `code` directory of your model artifact", which is where a
/// framework container looks for it.
pub const INFERENCE_CODE_DIRECTORY: &str = "code";

/// The suffix of an artifact entry that is Python source, which is the language the handler
/// script is written in.
pub const PYTHON_SUFFIX: &str = ".py";

/// The entry point a framework inference container runs when its `SAGEMAKER_PROGRAM` names none:
/// the conventional name of the handler script.
pub const DEFAULT_INFERENCE_PROGRAM: &str = "inference.py";

/// The container environment variable naming the entry point: "the environment variables to set
/// in the Docker container" (`ContainerDefinition$Environment`) carry it for every framework
/// container, and its value is the script's file name inside the code directory.
pub const SAGEMAKER_PROGRAM_VARIABLE: &str = "SAGEMAKER_PROGRAM";

/// The module the kernel is handed to the interpreter as, and the entry point it resolves in it.
const INFERENCE_KERNEL_MODULE: &str = "sagemaker_inference_kernel";

/// The most Python modules of one artifact's code directory a run is given.
///
/// An EMULATOR ceiling: the artifact is a caller's own object, so how much of it reaches a run is
/// bounded here.
pub const MAX_SCRIPT_MODULES: usize = 32;

/// The most bytes of the model directory one run is handed, across every entry.
///
/// An EMULATOR ceiling, sized at the bound the artifact's own reading already enforces over the
/// whole inflated archive ([`super::MAX_INFLATED_ARCHIVE_BYTES`]), so what one run is handed is
/// everything that reading produced: an artifact is handed over IN FULL or not at all, because a
/// model directory with entries missing would let this emulator's bound surface as the script's
/// own "no such file" failure.
pub const MAX_MODEL_DIRECTORY_BYTES: usize = super::MAX_INFLATED_ARCHIVE_BYTES as usize;

/// The most bytes of payload one run is handed.
///
/// An EMULATOR ceiling, sized at the largest payload the models admit for an inference, so a
/// request the modelled bounds accept is not refused here: `InvokeEndpoint$Body` is `@length
/// 0..6291456`, and a transform job's own records arrive one object at a time under the ceiling
/// an object is downloaded with ([`super::MAX_OBJECT_BYTES`]).
pub const MAX_SCRIPT_PAYLOAD_BYTES: usize = 6 * 1024 * 1024;

/// How many inference scripts run at once in this process.
///
/// CHOSEN: a run stands an interpreter up and holds a thread of this process, so without this the
/// bounds below would be multiplied by the number of invocations a client sends at once.
pub const MAX_CONCURRENT_INFERENCE_SCRIPTS: usize = 2;

/// How long one request waits for one of [`MAX_CONCURRENT_INFERENCE_SCRIPTS`], at most.
///
/// CHOSEN: the wait is bounded, so the request is answered either way. A request that waited the
/// whole wait ran NOTHING, and is answered with exactly that rather than with an inference nothing
/// computed.
pub const INFERENCE_SCRIPT_SLOT_WAIT: Duration = Duration::from_secs(10);

/// The longest ONE inference script may run in the RUN'S OWN time.
///
/// CHOSEN, not an AWS duration: the run happens inside the request that needs its answer, so this
/// is the span this emulator gives one. It is a span of the RUN's own time rather than of this
/// host's — read against a clock the run itself drives ([`INFERENCE_SCRIPT_CLOCK_TICK`]) — so what
/// a request is answered is the same on an idle host and on a loaded one. What the run may take of
/// THIS host is bounded beside it ([`INFERENCE_SCRIPT_HOST_CEILING`]).
pub const INFERENCE_SCRIPT_RUN_CEILING: Duration = Duration::from_secs(10);

/// How far ONE reading of a run's clock moves it.
///
/// The span above is spent by what the RUN does, and a traced event is one of those. The tick is
/// sized so that the whole step budget ([`MAX_SCRIPT_STEPS`]) spends well under that span, which
/// is what leaves a script that computes without end to be answered by the step budget — a count
/// of its OWN work — rather than by a clock.
pub const INFERENCE_SCRIPT_CLOCK_TICK: Duration = Duration::from_nanos(100);

/// How much of THIS HOST's own time ONE run may take, past which the engine stops it whatever its
/// own clock reads.
///
/// CHOSEN: the span above is the RUN's own time, so it bounds no CPU of this host — a script
/// spending traced events, or sitting in one long native call that delivers none, would hold a
/// thread of this process for as long as the host took over it. This is the bound that stops it,
/// read from this host's own clock at every traced event and from the host's own watch over the
/// run. It is generous beside the spans the engine's phases take in the run's own time
/// (`INIT_PHASE_LIMIT` standing the interpreter up, then the span above), because a script that
/// FINISHED must be reported on what it came to rather than on this host's load.
pub const INFERENCE_SCRIPT_HOST_CEILING: Duration = INIT_PHASE_LIMIT
    .saturating_add(INFERENCE_SCRIPT_RUN_CEILING)
    .saturating_mul(2);

/// How long the engine's own stop is given to land, once the host ceiling has latched it.
pub const INFERENCE_SCRIPT_STOP_GRACE: Duration = Duration::from_secs(5);

/// How long the caller waits for the thread one run happens on.
///
/// The host ceiling plus the grace the engine's stop needs: the stop is latched by the host and
/// raised at the next traced event the interpreter delivers, so a thread the engine has stopped
/// answers a moment after the ceiling rather than at it. A thread that has still not answered by
/// then is one this emulator gave up on, and the request says so.
pub const INFERENCE_SCRIPT_HOST_WAIT: Duration =
    INFERENCE_SCRIPT_HOST_CEILING.saturating_add(INFERENCE_SCRIPT_STOP_GRACE);

/// How many traced events one run may execute.
///
/// CHOSEN: large enough for an inference script and for the frozen standard library's own import
/// chain, which spends a large share of it before the script's first line, and far below what the
/// span the run's clock gives it allows for — so a script that loops without end is answered by
/// THIS budget. It is a count of the script's own work, so it is the same count on every host.
pub const MAX_SCRIPT_STEPS: u64 = 20_000_000;

/// How much memory one run may add to this process. CHOSEN, for the same reason.
const MAX_SCRIPT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// How deep one run may recurse. The interpreter's own default is below what the frozen standard
/// library's import chain needs, so this is the limit a run is given; the call that would raise it
/// is taken away by the engine before any script code runs.
const MAX_SCRIPT_RECURSION: usize = 2_000;

/// How much of what a script printed is kept. It reaches no response — an invocation answers what
/// the script returned, not what it printed — so this only bounds what one run holds in memory.
const MAX_SCRIPT_LOG_BYTES: usize = 64 * 1024;

/// How large the value one run may answer with is: the rendered inference, encoded.
///
/// Sized above the largest payload a run is handed ([`MAX_SCRIPT_PAYLOAD_BYTES`]) and its
/// encoding, so a script that answers with what it was given — which a passthrough model does —
/// is not stopped for it.
const MAX_SCRIPT_ANSWER_BYTES: usize = 16 * 1024 * 1024;

/// How many BYTES of what a run REPORTED about itself — the message a script raised, the detail of
/// a run nothing performed — are kept, and how many of the exception CLASS beside it.
///
/// EMULATOR ceilings on a caller-chosen string: a script decides what it raises, so a condition
/// reported for it may not carry a string whose size the request chose into a response, into the
/// store, or into a log. The two are budgeted so that the whole condition — the sentence
/// [`ServingRefusal::message`] composes from them, inside the class a batch transform job's
/// `FailureReason` wraps it in — fits the length the modelled member admits
/// (`DescribeTransformJob`'s `FailureReason` is `@length 0..1024`), with room to spare for that
/// fixed text.
pub const MAX_SCRIPT_DETAIL_BYTES: usize = 768;
/// See [`MAX_SCRIPT_DETAIL_BYTES`]: an exception class is a Python identifier, so a short bound
/// leaves the message the greater share of the budget.
pub const MAX_SCRIPT_ERROR_CLASS_BYTES: usize = 64;

/// How many bytes of a media type a run NAMES for its answer are admitted.
///
/// A media type is a short token pair, and the modelled member the answer carries it through is
/// `@length 0..1024` in printable ASCII (`InvokeEndpoint`'s `ContentType` header), so a value past
/// this is not one ([`answered_media_type`] falls back instead of answering it).
pub const MAX_MEDIA_TYPE_BYTES: usize = 255;

/// The stack the thread one run happens on is given: well above the interpreter's clamped
/// recursion limit, so deep recursion in a script ends that run with the interpreter's own
/// `RecursionError` rather than touching the thread's guard page.
const INFERENCE_SCRIPT_STACK_BYTES: usize = 16 * 1024 * 1024;

/// The INFERENCE CODE one model artifact carries: the entry point the container runs, the modules
/// the code directory holds, and the files the model directory holds beside them.
#[derive(Debug, Clone)]
pub struct InferenceScript {
    /// The entry point, as the name it is imported under (the file name without its suffix).
    pub program: String,
    /// Every Python module of the artifact's code directory, as `(path under that directory,
    /// source)` — so a script that imports a sibling module of its own resolves it.
    pub modules: Vec<(String, String)>,
    /// The artifact's other entries, as the files the model directory holds: `(path under
    /// [`super::MODEL_DIRECTORY`], contents)`.
    pub model_files: Vec<(String, Vec<u8>)>,
}

impl InferenceScript {
    /// What one run of this script is handed: its own modules, under the paths they are imported
    /// by, plus the kernel that drives them.
    fn package(&self) -> Vec<PackageModule> {
        let mut modules: Vec<PackageModule> = self
            .modules
            .iter()
            .map(|(path, source)| PackageModule {
                path: path.clone(),
                source: source.clone(),
            })
            .collect();
        modules.push(PackageModule {
            path: format!("{INFERENCE_KERNEL_MODULE}{PYTHON_SUFFIX}"),
            source: INFERENCE_KERNEL_SOURCE.to_string(),
        });
        modules
    }
}

/// The inference code `entries` — the entries of one model artifact — carries, and `None` for an
/// artifact that carries none.
///
/// `program` is the entry point the model's container names (`SAGEMAKER_PROGRAM`); an artifact
/// whose container names none is read for [`DEFAULT_INFERENCE_PROGRAM`], the conventional name.
/// The code directory's own entries are the modules; everything else the artifact holds is the
/// model directory the script reads its parameters out of.
#[must_use]
pub fn inference_script_in(
    entries: &[(String, Vec<u8>)],
    program: Option<&str>,
) -> Option<InferenceScript> {
    let directory = format!("{INFERENCE_CODE_DIRECTORY}/");
    let named = program
        .map(str::trim)
        .filter(|named| !named.is_empty())
        .unwrap_or(DEFAULT_INFERENCE_PROGRAM)
        .trim_start_matches("./");
    // The container names the entry point by file name; a value that already carries the code
    // directory or omits the suffix still names the same module.
    let named = named.strip_prefix(&directory).unwrap_or(named);
    let entry_point = match named.strip_suffix(PYTHON_SUFFIX) {
        Some(stem) => stem.to_string(),
        None => named.to_string(),
    };
    let mut modules: Vec<(String, String)> = Vec::new();
    let mut model_files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut held = 0_usize;
    for (path, bytes) in entries {
        let path = path.trim_start_matches("./");
        match path.strip_prefix(&directory) {
            Some(under) if under.ends_with(PYTHON_SUFFIX) => {
                if modules.len() < MAX_SCRIPT_MODULES {
                    if let Ok(source) = ::std::str::from_utf8(bytes) {
                        modules.push((under.to_string(), source.to_string()));
                    }
                }
            }
            // Everything else the artifact holds is what the model directory holds: the
            // parameters the script's own `model_fn` reads, under the path the artifact stored
            // them at. Past the bound the run is handed NOTHING rather than a model directory
            // with entries missing, which would surface as the script's own failure to open one.
            _ => {
                held = held.saturating_add(bytes.len());
                if held > MAX_MODEL_DIRECTORY_BYTES {
                    return None;
                }
                model_files.push((path.to_string(), bytes.clone()));
            }
        }
    }
    // An artifact is served by its code only when the entry point the container runs is really
    // there: a code directory holding other modules but not that one is a container whose own
    // import fails, which is no inference this emulator can compute.
    let resolved = format!("{entry_point}{PYTHON_SUFFIX}");
    let carried = modules.iter().any(|(path, _)| *path == resolved);
    carried.then_some(InferenceScript {
        program: entry_point,
        modules,
        model_files,
    })
}

/// Run `script` over `body` and answer what its own handler functions returned.
///
/// Synchronous: the run happens on a thread of its own and this call waits for its answer, so
/// every caller of the one inference reading ([`super::serve_inference`]) reaches it the same way,
/// off the request worker.
///
/// `Err` is the condition that stopped the inference, which the operation that asked for it
/// reports as its own modelled failure — never an answer nothing computed.
pub fn run_inference_script(
    script: &InferenceScript,
    body: &[u8],
    content_type: Option<&str>,
    accept: Option<&str>,
) -> Result<ServedInference, ServingRefusal> {
    if body.len() > MAX_SCRIPT_PAYLOAD_BYTES {
        return Err(ServingRefusal::PayloadTooLarge { bytes: body.len() });
    }
    let event = run_event(script, body, content_type, accept)
        .ok_or_else(|| ServingRefusal::ScriptNotRun {
            detail: "the payload could not be handed to the model's inference script".to_string(),
        })?;
    // Nothing is run at all while this process is already running as many scripts as it may.
    let Some(slot) = INFERENCE_SCRIPT_SLOTS.take(INFERENCE_SCRIPT_SLOT_WAIT) else {
        return Err(ServingRefusal::ScriptNotRun {
            detail: format!(
                "this emulator runs at most {MAX_CONCURRENT_INFERENCE_SCRIPTS} inference scripts \
                 at a time and none was free"
            ),
        });
    };
    let modules = script.package();
    let handler = format!("{INFERENCE_KERNEL_MODULE}.serve");
    let (sender, receiver) = ::std::sync::mpsc::channel();
    let spawned = ::std::thread::Builder::new()
        .name("sagemaker-inference-script".to_string())
        .stack_size(INFERENCE_SCRIPT_STACK_BYTES)
        .spawn(move || {
            // Held for as long as the run lasts, whoever is still waiting for it: a run this
            // caller has given up on keeps its slot until its thread really ends.
            let _slot = slot;
            let finished = python::run_against(
                &python::Run {
                    modules: &modules,
                    handler: &handler,
                    // The script is handed its payload and its model directory by the kernel, so
                    // the run carries no environment of its own and nothing of this process's.
                    environment: &::std::collections::BTreeMap::new(),
                    // The instruction set the environment the script runs in has. A model names
                    // none, so it is the one the engine's own runtime reports when none is named.
                    architecture:
                        ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                    event: Some(&event),
                    request_id: "",
                    invoked_function_arn: "",
                    client_context: None,
                    tenant_id: None,
                    budget: python::Budget {
                        wall_clock: INFERENCE_SCRIPT_RUN_CEILING,
                        steps: MAX_SCRIPT_STEPS,
                        memory_bytes: MAX_SCRIPT_MEMORY_BYTES,
                        log_bytes: MAX_SCRIPT_LOG_BYTES,
                        response_bytes: MAX_SCRIPT_ANSWER_BYTES,
                        recursion_limit: MAX_SCRIPT_RECURSION,
                    },
                    // An inference container reaches no other service of this emulator: the run
                    // is given no seam to dispatch an AWS call on.
                    aws: None,
                    // It is no Lambda durable execution's run either, so it takes no step of one
                    // and checkpoints nothing onto one.
                    durable: None,
                },
                // The clock the run's own span is read against: one the RUN drives, a tick per
                // traced event it delivers, stating what the run may take of THIS host besides —
                // the bound the engine stops it on whatever its own clock reads.
                &python::Clock::driven(INFERENCE_SCRIPT_CLOCK_TICK)
                    .within(INFERENCE_SCRIPT_HOST_CEILING),
            );
            let _ = sender.send(finished);
        });
    if spawned.is_err() {
        return Err(ServingRefusal::ScriptNotRun {
            detail: "the inference script could not be given a thread to run on".to_string(),
        });
    }
    let finished = match receiver.recv_timeout(INFERENCE_SCRIPT_HOST_WAIT) {
        Ok(finished) => finished,
        // The thread has still not answered a grace period past the host time one run may take —
        // which the engine has by then stopped the run on — so this emulator gave up on it.
        Err(_) => {
            return Err(ServingRefusal::ScriptNotRun {
                detail: "the model's inference script did not answer and was given up on"
                    .to_string(),
            })
        }
    };
    read_answer(&finished.completion, accept)
}

/// What the request reports for a run the interpreter answered for.
///
/// Every string a run contributes here is the SCRIPT's own — its exception class and message — so
/// each is clamped before it leaves this boundary ([`MAX_SCRIPT_DETAIL_BYTES`],
/// [`MAX_SCRIPT_ERROR_CLASS_BYTES`]): the condition is reported in an answer and stored on the job
/// that asked for it, and neither may carry a string whose length the script chose.
fn read_answer(
    completion: &python::Completion,
    accept: Option<&str>,
) -> Result<ServedInference, ServingRefusal> {
    match completion {
        python::Completion::Returned(answer) => rendered_answer(answer, accept),
        // A run bounded by this emulator answers ONE value, so nothing of an inference streams.
        python::Completion::Streamed { .. } => Err(ServingRefusal::ScriptNotRun {
            detail: "the model's inference script answered no single value".to_string(),
        }),
        python::Completion::Raised {
            error_type,
            error_message,
            ..
        } => Err(ServingRefusal::ScriptRaised {
            error_type: clamped(error_type, MAX_SCRIPT_ERROR_CLASS_BYTES),
            error_message: clamped_detail(error_message),
        }),
        python::Completion::Stopped(stop) => Err(ServingRefusal::ScriptStopped {
            reason: clamped_detail(stop.reason()),
        }),
        python::Completion::Internal(detail) => Err(ServingRefusal::ScriptNotRun {
            detail: clamped_detail(detail),
        }),
    }
}

/// `text` within [`MAX_SCRIPT_DETAIL_BYTES`].
///
/// The script chooses what it raises, so the length of what it raised is the request's choice: a
/// condition reported for one run is bounded HERE, before it reaches a response, a stored
/// `FailureReason` or a log, rather than being bounded only by what one live run may allocate.
fn clamped_detail(text: &str) -> String {
    clamped(text, MAX_SCRIPT_DETAIL_BYTES)
}

/// `text` within `bound` BYTES, cut back to the character boundary at or below it — so the bound
/// is the one the modelled member states, in the units it states it in, and the text stays text.
fn clamped(text: &str, bound: usize) -> String {
    if text.len() <= bound {
        return text.to_string();
    }
    let mut at = bound;
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    text[..at].to_string()
}

/// The inference the kernel ANSWERED, read out of the document it answered with.
fn rendered_answer(
    answer: &str,
    accept: Option<&str>,
) -> Result<ServedInference, ServingRefusal> {
    let unreadable = || ServingRefusal::ScriptNotRun {
        detail: "the answer of the model's inference script could not be read".to_string(),
    };
    let document: ::serde_json::Value = ::serde_json::from_str(answer).map_err(|_| unreadable())?;
    let encoded = document
        .get("body")
        .and_then(::serde_json::Value::as_str)
        .ok_or_else(unreadable)?;
    let body = ::aws_smithy_types::base64::decode(encoded).map_err(|_| unreadable())?;
    // The media type the SCRIPT named, and otherwise the one the request asked for — which is what
    // a container that renders what it was asked for answers in.
    let named = document
        .get("contentType")
        .and_then(::serde_json::Value::as_str);
    Ok(ServedInference {
        body,
        content_type: answered_media_type(named, accept),
    })
}

/// The media type the ANSWER carries, as a client is handed it: the one the script named when that
/// is a media type at all, and otherwise the one the request asked for.
///
/// Both are strings a caller chose — the script's answer and the request's `Accept` — and the
/// member they reach is a header the model constrains (`InvokeEndpoint`'s `ContentType`,
/// `@length 0..1024`, printable ASCII), so neither is answered unless it is a media type
/// ([`is_media_type`]). A real inference container answers a well-formed one; a value that is not
/// one names no type at all, and the type a SageMaker inference container answers records in is
/// what the answer then carries.
fn answered_media_type(named: Option<&str>, accept: Option<&str>) -> String {
    named
        .map(str::trim)
        .filter(|named| is_media_type(named))
        .map(str::to_string)
        .unwrap_or_else(|| requested_media_type(accept))
}

/// Whether `named` IS a media type: `type/subtype`, optionally followed by parameters, in
/// printable ASCII and no longer than [`MAX_MEDIA_TYPE_BYTES`].
///
/// The two names are matched against the token grammar a media type's own definition states, so a
/// value carrying a control character, a line break or a length nothing bounds is not one.
fn is_media_type(named: &str) -> bool {
    if named.is_empty() || named.len() > MAX_MEDIA_TYPE_BYTES {
        return false;
    }
    if !named
        .chars()
        .all(|character| character.is_ascii_graphic() || character == ' ')
    {
        return false;
    }
    let Some((kind, rest)) = named.split_once('/') else {
        return false;
    };
    let subtype = rest.split(';').next().unwrap_or(rest).trim();
    is_media_type_token(kind.trim()) && is_media_type_token(subtype)
}

/// One name of a media type: the token characters its own grammar admits.
fn is_media_type_token(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(character)
        })
}

/// The media type an answer the script named none for carries: the one the request asked for when
/// it asked for a particular one, and otherwise the type a SageMaker inference container answers
/// records in.
///
/// A request asking for several types, or for any type at all, has asked for no particular one, so
/// the answer carries the type its records are in.
fn requested_media_type(accept: Option<&str>) -> String {
    match accept
        .map(str::trim)
        .filter(|accept| !accept.contains('*') && !accept.contains(',') && is_media_type(accept))
    {
        Some(accept) => accept.to_string(),
        None => CSV_MEDIA_TYPE.to_string(),
    }
}

/// The EVENT one run is handed: the entry point to import, the model directory to read, and the
/// payload to serve. The payload and every file travel as base64, so bytes that are not text reach
/// the script exactly as Amazon S3 holds them.
fn run_event(
    script: &InferenceScript,
    body: &[u8],
    content_type: Option<&str>,
    accept: Option<&str>,
) -> Option<String> {
    let files: ::serde_json::Map<String, ::serde_json::Value> = script
        .model_files
        .iter()
        .map(|(path, bytes)| {
            (
                path.clone(),
                ::serde_json::Value::String(::aws_smithy_types::base64::encode(bytes)),
            )
        })
        .collect();
    let event = ::serde_json::json!({
        "program": script.program,
        "modelDir": MODEL_DIRECTORY,
        "modelFiles": files,
        "body": ::aws_smithy_types::base64::encode(body),
        "contentType": content_type,
        "accept": accept,
    });
    ::serde_json::to_string(&event).ok()
}

/// The runs this process has given a slot to, out of [`MAX_CONCURRENT_INFERENCE_SCRIPTS`].
static INFERENCE_SCRIPT_SLOTS: RunSlots = RunSlots {
    taken: Mutex::new(0),
    freed: Condvar::new(),
};

/// The slots the concurrent-run count hands out.
///
/// A counting semaphore this caller can wait on without an async runtime, because the run it
/// guards is performed synchronously on a thread of its own: the slot is MOVED INTO that thread,
/// so it is given back when the thread really ends — including a run the caller has already given
/// up waiting for, which would otherwise take the count past its bound while its thread was still
/// going.
#[derive(Debug)]
struct RunSlots {
    /// How many runs hold a slot.
    taken: Mutex<usize>,
    /// Signalled as each slot is given back.
    freed: Condvar,
}

impl RunSlots {
    /// Take a slot, waiting at most `wait` for one — and `None` when none came free, which is a
    /// run that did not happen at all.
    fn take(&'static self, wait: Duration) -> Option<RunSlot> {
        let deadline = ::std::time::Instant::now() + wait;
        let mut taken = self.taken.lock().unwrap_or_else(|held| held.into_inner());
        loop {
            if *taken < MAX_CONCURRENT_INFERENCE_SCRIPTS {
                *taken += 1;
                return Some(RunSlot { slots: self });
            }
            let left = deadline.saturating_duration_since(::std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            let (held, _) = self
                .freed
                .wait_timeout(taken, left)
                .unwrap_or_else(|held| held.into_inner());
            taken = held;
        }
    }
}

/// One taken slot, given back when the run that holds it ends.
#[derive(Debug)]
struct RunSlot {
    slots: &'static RunSlots,
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        let mut taken = self
            .slots
            .taken
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        *taken = taken.saturating_sub(1);
        drop(taken);
        self.slots.freed.notify_one();
    }
}

/// The KERNEL one run resolves: the program that imports the artifact's own entry point and calls
/// its handler functions in the order an inference container calls them.
///
/// Ordinary Python, the way every module this workspace supplies an interpreter is. It is NOT the
/// containment — that is the engine's, in host code no script can reach — it is the CONTAINER's
/// contract: `model_fn` then `input_fn` then `predict_fn` then `output_fn`, with `transform_fn`
/// replacing the last three where the script defines one, over a model directory that is the
/// artifact's own entries and nothing else.
const INFERENCE_KERNEL_SOURCE: &str = r#"
"""The inference an artifact's own handler script computes for one payload."""

import base64
import builtins
import importlib
import io
import json


def _files_of(event):
    """The model directory as the run was handed it, keyed by its own paths."""
    held = {}
    for path, encoded in (event.get("modelFiles") or {}).items():
        held[path] = base64.b64decode(encoded)
    return held


def _opener(files, model_dir):
    """`open` over the model directory: the only filesystem the script sees."""
    root = model_dir.rstrip("/") + "/"

    def path_of(named):
        if isinstance(named, bytes):
            named = named.decode("utf-8", "surrogateescape")
        elif not isinstance(named, str):
            held = getattr(named, "__fspath__", None)
            if held is None:
                raise TypeError(
                    "expected str, bytes or os.PathLike object, not " + type(named).__name__
                )
            return path_of(held())
        if named.startswith(root):
            named = named[len(root):]
        return named.lstrip("/")

    def open_file(
        file,
        mode="r",
        buffering=-1,
        encoding=None,
        errors=None,
        newline=None,
        closefd=True,
        opener=None,
    ):
        path = path_of(file)
        if any(flag in mode for flag in ("w", "a", "x", "+")):
            # The model directory a container reads is the artifact itself, and nothing a run
            # writes is kept anywhere, so it is read-only rather than silently accepted.
            raise OSError(30, "Read-only file system", path)
        held = files.get(path)
        if held is None:
            raise FileNotFoundError(2, "No such file or directory", path)
        if "b" in mode:
            return io.BytesIO(held)
        return io.StringIO(held.decode(encoding or "utf-8", errors or "strict"))

    return open_file


def _model(module, model_dir):
    """The model the script's own `model_fn` loads, and None for a script that defines none."""
    model_fn = getattr(module, "model_fn", None)
    if model_fn is None:
        return None
    return model_fn(model_dir)


def _rendered(answered):
    """The bytes the container answers with, and the media type the script named for them."""
    media = None
    # A handler that renders its own answer may name the type it rendered it in beside it, which
    # is the pair form `output_fn` answers with.
    if isinstance(answered, tuple) and len(answered) == 2:
        answered, media = answered[0], answered[1]
        if media is not None and not isinstance(media, str):
            media = None
    if isinstance(answered, (bytes, bytearray)):
        return bytes(answered), media
    if isinstance(answered, str):
        return answered.encode("utf-8"), media
    return json.dumps(answered).encode("utf-8"), media or "application/json"


def serve(event, context):
    model_dir = event.get("modelDir") or ""
    # The script reads its own files through `open`, which is this one over the artifact's
    # entries: the engine has already taken away every way of opening a file of the host.
    builtins.open = _opener(_files_of(event), model_dir)
    module = importlib.import_module(event["program"])
    body = base64.b64decode(event.get("body") or "")
    content_type = event.get("contentType")
    accept = event.get("accept")
    transform_fn = getattr(module, "transform_fn", None)
    if transform_fn is not None:
        answered = transform_fn(_model(module, model_dir), body, content_type, accept)
    else:
        model = _model(module, model_dir)
        input_fn = getattr(module, "input_fn", None)
        data = input_fn(body, content_type) if input_fn is not None else body
        predict_fn = getattr(module, "predict_fn", None)
        if predict_fn is None:
            raise AttributeError(
                "the inference script defines neither transform_fn nor predict_fn"
            )
        prediction = predict_fn(data, model)
        output_fn = getattr(module, "output_fn", None)
        answered = output_fn(prediction, accept) if output_fn is not None else prediction
    rendered, media = _rendered(answered)
    return {
        "body": base64.b64encode(rendered).decode("ascii"),
        "contentType": media,
    }
"#;
