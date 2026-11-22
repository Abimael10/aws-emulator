// SPDX-License-Identifier: CC0-1.0

//! The PYTHON program a job's container declares, performed over the container's own storage.
//!
//! # The rule this module implements
//!
//! `AlgorithmSpecification$ContainerEntrypoint` is "the entrypoint script for a Docker
//! container used to run a training job. This script takes precedence over the default train
//! processing instructions", `AppSpecification$ContainerEntrypoint` is "the entrypoint for a
//! container used to run a processing job", and each is followed by that specification's
//! `ContainerArguments` — together the argument vector the image runs. SageMaker's own
//! algorithm, framework and deep-learning images are Python environments, so the program such
//! a vector names is most often the PYTHON INTERPRETER with a program for it: `python3 -c
//! <source>`, or `python3 <script>` with the script in the image or in the job's own data. A
//! job declaring one of those is therefore a job whose work IS that Python program, and the
//! status the job ends in is the status the program exits with
//! ([`super::container_run`] states that rule for every declared program).
//!
//! So that is what happens here: the declared interpreter's program is RUN, and the files it
//! left are what the job's own output stages upload — the model artifacts a training container
//! writes to [`super::MODEL_DIRECTORY`] ("a SageMaker container stores your trained model
//! artifacts in the /opt/ml/model directory"), and the contents of the `LocalPath` a
//! processing job's `ProcessingS3Output` names.
//!
//! # The engine is NOT this module's, and the CONTAINER is ONE RUN's
//!
//! No execution substrate is introduced here. The interpreter is
//! [`::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python`] — the ONE
//! contained RustPython this workspace embeds for running caller-supplied Python, driven
//! exactly as this crate already drives it for the inference code a model artifact carries
//! ([`super::inference_script`]). "One" is one IMPLEMENTATION, never one shared instance: each
//! call of [`run_python_container_program`] stands an interpreter up for THAT run, builds the
//! container's storage from what it was handed, and tears both down when it answers — so a
//! job's program sees the data the service placed in ITS container and the files its OWN
//! program wrote, and nothing of a job that ran before it.
//!
//! Everything that CONTAINS a run lives in that one module and
//! is maintained once: its native modules are an allow-list with no socket, subprocess,
//! memory-map, native-library or thread module in it, the operating-system modules a program
//! can import are that crate's own in-memory ones with every host-reaching call refused, every
//! way of opening a file of the HOST is taken away before a line of guest code runs, and the
//! deadline, step, memory, recursion and answer bounds are enforced in host code the program
//! cannot reach — including the latch that makes a stopped run's answer that stop whatever the
//! program catches.
//!
//! What this module adds is the CONTAINER's own wiring, and nothing else:
//!
//! * the ENTRY module ([`CONTAINER_KERNEL_SOURCE`]) — ordinary Python, handed to the
//!   interpreter as a module of the run the way every other module this workspace supplies one
//!   is: it runs the declared program as `__main__`, with the interpreter's own `sys.argv`, and
//!   reports the status it exited with, the two console streams it wrote and the files it left;
//! * the container's own STORAGE: an in-memory tree THIS RUN owns, seeded with the data the
//!   SERVICE places in the container before its entrypoint is invoked (a training channel's
//!   objects under `/opt/ml/input/data/<ChannelName>/`, a processing input's objects under its
//!   `LocalPath`) and read back after the run, served to the program through `open` and the
//!   directory readings of `os` — the same per-run in-memory tree the shell session performs a
//!   `/bin/sh` program's file utilities over ([`super::container_run`]). No path of the HOST
//!   resolves in it: the engine has already taken every host file call away, and this tree holds
//!   only what the service put there for this job and what this program itself wrote, so no
//!   path a request names reaches another job's data;
//! * the host-side bounds around the run: it happens on a thread of its own with the stack a run
//!   of caller-supplied code is given, under this process's own count of concurrent runs
//!   ([`super::run_slots`]), and the caller waits for its answer no longer than the host time one
//!   run may take plus the grace the engine's stop needs.
//!
//! # A program nothing performed CLAIMS NOTHING
//!
//! A vector naming the interpreter with no program for it, a run this process could not carry
//! out at all, and a run one of this emulator's own bounds stopped are each
//! [`ContainerOutcome::NotPerformed`]: no exit status and no output is claimed for them,
//! because the container never exited. Only a program that really ran to its end reports the
//! status it exited with.

use super::*;

use ::std::time::Duration;

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::INIT_PHASE_LIMIT;

use super::run_slots::RunSlots;

/// The module the entry program is handed to the interpreter as, and the function in it the
/// interpreter calls.
const CONTAINER_KERNEL_MODULE: &str = "sagemaker_container_program";
/// See [`CONTAINER_KERNEL_MODULE`].
const CONTAINER_KERNEL_FUNCTION: &str = "run";

/// The name `sys.argv[0]` carries for a program given on the command line itself, which is
/// what the interpreter puts there for `-c` (`python3 -c <source>`).
const COMMAND_ARGUMENT: &str = "-c";

/// How many container programs this process runs at once.
///
/// CHOSEN: a run stands an interpreter up and holds a thread of this process, so without this
/// the ceilings one run is given would be multiplied by the jobs a client creates at once.
pub const MAX_CONCURRENT_CONTAINER_PROGRAMS: usize = 2;

/// How long one job waits for one of [`MAX_CONCURRENT_CONTAINER_PROGRAMS`], at most.
///
/// CHOSEN: the wait is bounded, so the job is always answered. A job that waited the whole wait
/// ran NOTHING, and reports exactly that rather than a status no program produced.
pub const CONTAINER_PROGRAM_SLOT_WAIT: Duration = Duration::from_secs(20);

/// The longest ONE container program may run in the RUN'S OWN time.
///
/// CHOSEN, not an AWS duration (a real training container runs for as long as its
/// `StoppingCondition` allows): it is a span of the RUN's own time, read against a clock the
/// run itself drives ([`CONTAINER_PROGRAM_CLOCK_TICK`]), so what a job comes to rest in is the
/// same on an idle host and on a loaded one. What the run may take of THIS host is bounded
/// beside it ([`CONTAINER_PROGRAM_HOST_CEILING`]).
pub const CONTAINER_PROGRAM_RUN_CEILING: Duration = Duration::from_secs(10);

/// How far ONE reading of a run's clock moves it.
///
/// The span above is spent by what the RUN does, and a traced event is one of those. The tick
/// is sized so that the whole step budget ([`MAX_CONTAINER_PROGRAM_STEPS`]) spends well under
/// that span, which is what leaves a program that computes without end to be answered by the
/// step budget — a count of its OWN work — rather than by a clock.
pub const CONTAINER_PROGRAM_CLOCK_TICK: Duration = Duration::from_nanos(100);

/// How much of THIS HOST's own time ONE run may take, past which the engine stops it whatever
/// its own clock reads.
///
/// CHOSEN: the span above is the RUN's own time, so it bounds no CPU of this host — a program
/// spending traced events, or sitting in one long native call that delivers none, would hold a
/// thread of this process for as long as the host took over it. This is the bound that stops
/// it, read from this host's own clock at every traced event and from the host's own watch over
/// the run.
pub const CONTAINER_PROGRAM_HOST_CEILING: Duration = INIT_PHASE_LIMIT
    .saturating_add(CONTAINER_PROGRAM_RUN_CEILING)
    .saturating_mul(2);

/// How long the engine's own stop is given to land, once the host ceiling has latched it.
const CONTAINER_PROGRAM_STOP_GRACE: Duration = Duration::from_secs(5);

/// How long the job waits for the thread one run happens on: the host ceiling plus the grace
/// the engine's stop needs. A thread that has still not answered by then is one this emulator
/// gave up on, and the job says so.
const CONTAINER_PROGRAM_HOST_WAIT: Duration =
    CONTAINER_PROGRAM_HOST_CEILING.saturating_add(CONTAINER_PROGRAM_STOP_GRACE);

/// How many traced events one run may execute.
///
/// CHOSEN: large enough for a training or processing script and for the frozen standard
/// library's own import chain, which spends a large share of it before the program's first
/// line, and far below what the span the run's clock gives it allows for — so a program that
/// loops without end is answered by THIS budget. It is a count of the program's own work, so it
/// is the same count on every host.
pub const MAX_CONTAINER_PROGRAM_STEPS: u64 = 20_000_000;

/// How much memory one run may add to this process. CHOSEN, for the same reason.
const MAX_CONTAINER_PROGRAM_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// How much one run may write to the `/tmp` of the environment it runs in. CHOSEN: that
/// directory is a disk in a real container and is held in this process's memory here.
const MAX_CONTAINER_PROGRAM_SCRATCH_BYTES: u64 = 64 * 1024 * 1024;

/// How deep one run may recurse. The interpreter's own default is below what the frozen
/// standard library's import chain needs, so this is the limit a run is given; the call that
/// would raise it is taken away by the engine before any program code runs.
const MAX_CONTAINER_PROGRAM_RECURSION: usize = 2_000;

/// How much of what the program printed is kept by the interpreter's own log.
const MAX_CONTAINER_PROGRAM_LOG_BYTES: usize = 64 * 1024;

/// How large the value one run may answer with: the status the program exited with, the tails
/// of its two console streams, and the files it left in the container's storage, encoded.
///
/// This is the HOST's bound on what one container program may leave behind: a run whose answer
/// is larger is stopped by the engine on this bound and reports no status at all, so no
/// unbounded amount of caller-written data ever reaches this service's state or Amazon S3.
const MAX_CONTAINER_PROGRAM_ANSWER_BYTES: usize = 8 * 1024 * 1024;

/// How many characters of each console stream the entry module keeps.
const MAX_CONTAINER_PROGRAM_STREAM_CHARACTERS: usize = 64 * 1024;

/// The stack the thread one run happens on is given: well above the interpreter's clamped
/// recursion limit, so deep recursion in a program ends that run with the interpreter's own
/// `RecursionError` rather than touching the thread's guard page.
const CONTAINER_PROGRAM_STACK_BYTES: usize = 16 * 1024 * 1024;

/// The runs this process has given a slot to, out of
/// [`MAX_CONCURRENT_CONTAINER_PROGRAMS`].
static CONTAINER_PROGRAM_SLOTS: RunSlots = RunSlots::new();

/// Whether `program`'s first word names the PYTHON INTERPRETER, and the name it names it by.
///
/// A container entrypoint names its program by path (`/usr/bin/python3`) or by the name `PATH`
/// resolution would find it under (`python3`), so the final path component is the name. The
/// names are the ones a Python 3 environment provides for its interpreter — `python3`, the
/// version-qualified `python3.<minor>` a release installs beside it, and the unqualified
/// `python` the SageMaker images resolve to Python 3 — and nothing else: `python2` names an
/// interpreter this emulator has none of, so a vector naming it is a program nothing performs.
#[must_use]
pub fn python_interpreter(program: &[String]) -> Option<&str> {
    let executable = program.first()?;
    let name = executable.rsplit('/').next().unwrap_or(executable);
    let qualified = name
        .strip_prefix("python3.")
        .is_some_and(|minor| !minor.is_empty() && minor.chars().all(|digit| digit.is_ascii_digit()));
    (name == "python" || name == "python3" || qualified).then_some(name)
}

/// The PROGRAM an interpreter's argument vector names: the source given on the command line
/// itself, or the script file to run, with the arguments that follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonProgram {
    /// `python3 -c <source> [arguments]`: the program is the source, and `sys.argv[0]` is
    /// [`COMMAND_ARGUMENT`], which is what the interpreter puts there for this form.
    Command {
        source: String,
        arguments: Vec<String>,
    },
    /// `python3 <script> [arguments]`: the program is the file the path names, read out of the
    /// container's own storage, and `sys.argv[0]` is that path.
    Script {
        path: String,
        arguments: Vec<String>,
    },
}

impl PythonProgram {
    /// What `sys.argv[0]` carries for this form.
    fn argv0(&self) -> &str {
        match self {
            Self::Command { .. } => COMMAND_ARGUMENT,
            Self::Script { path, .. } => path,
        }
    }

    /// The arguments that follow the program on the command line.
    fn arguments(&self) -> &[String] {
        match self {
            Self::Command { arguments, .. } | Self::Script { arguments, .. } => arguments,
        }
    }

    /// The source to run, for the form that carries it.
    fn source(&self) -> Option<&str> {
        match self {
            Self::Command { source, .. } => Some(source),
            Self::Script { .. } => None,
        }
    }
}

/// The program the interpreter vector `program` names, and `None` for a vector that names the
/// interpreter and no program at all — the interactive interpreter, which a container's
/// entrypoint cannot be.
///
/// The words between the interpreter and its program are the interpreter's own options:
/// `-c <source>` IS the program, and every other option is one that changes nothing this
/// emulator does (buffering, byte-code writing, warnings) and is passed over, so a vector that
/// carries one still runs its program.
#[must_use]
pub fn python_program(program: &[String]) -> Option<PythonProgram> {
    let mut words = program.iter().skip(1);
    while let Some(word) = words.next() {
        if word == COMMAND_ARGUMENT {
            return Some(PythonProgram::Command {
                source: words.next()?.clone(),
                arguments: words.cloned().collect(),
            });
        }
        if word.starts_with('-') && word.len() > 1 {
            continue;
        }
        return Some(PythonProgram::Script {
            path: word.clone(),
            arguments: words.cloned().collect(),
        });
    }
    None
}

/// How this emulator states that a declared interpreter was given no program to run: nothing
/// was started, so no status is claimed for it.
#[must_use]
pub fn no_python_program(interpreter: &str) -> String {
    format!(
        "This emulator performs a job's declared container program itself, and the program \
         \"{interpreter}\" was given neither a script nor a {COMMAND_ARGUMENT} command to run, so \
         the container's program was never started and the job performed no work."
    )
}

/// How this emulator states that a program it DID start was stopped by one of its own bounds:
/// the program never exited, so no exit status is claimed for it either.
#[must_use]
pub fn program_stopped(bound: &str) -> String {
    format!(
        "This emulator performs a job's declared container program itself, bounded by what one \
         run may spend, and this program was stopped on {bound}, so it never exited and the job \
         has no result of its own."
    )
}

/// Run the Python program the interpreter vector `program` names, over a container storage
/// holding `files` — the data the service places in the container before its entrypoint is
/// invoked, as `(absolute path, contents)`.
///
/// Synchronous: the run happens on a thread of its own and this call waits for its answer, so
/// it is performed off the request worker by the one caller that performs a container program
/// ([`super::run_container_program`]).
///
/// `environment` is the job's own declared environment, which is what the program reads out of
/// `os.environ` — nothing of this process's.
#[must_use]
pub fn run_python_container_program(
    program: &[String],
    environment: Option<&HashMap<String, String>>,
    files: &[(String, String)],
) -> ContainerOutcome {
    let interpreter = python_interpreter(program).unwrap_or_default().to_string();
    let Some(form) = python_program(program) else {
        return ContainerOutcome::NotPerformed {
            reason: no_python_program(&interpreter),
        };
    };
    let Some(event) = run_event(&interpreter, &form, files) else {
        return ContainerOutcome::NotPerformed {
            reason: INTERNAL_FAILURE_REASON.to_string(),
        };
    };
    // Nothing is run at all while this process is already running as many programs as it may.
    let Some(slot) = CONTAINER_PROGRAM_SLOTS.take(
        MAX_CONCURRENT_CONTAINER_PROGRAMS,
        CONTAINER_PROGRAM_SLOT_WAIT,
    ) else {
        return ContainerOutcome::NotPerformed {
            reason: program_stopped(&format!(
                "the {MAX_CONCURRENT_CONTAINER_PROGRAMS} container programs this emulator runs \
                 at a time"
            )),
        };
    };
    let modules = vec![PackageModule {
        path: format!("{CONTAINER_KERNEL_MODULE}.py"),
        source: CONTAINER_KERNEL_SOURCE.to_string(),
    }];
    let handler = format!("{CONTAINER_KERNEL_MODULE}.{CONTAINER_KERNEL_FUNCTION}");
    let variables: ::std::collections::BTreeMap<String, String> = environment
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let (sender, receiver) = ::std::sync::mpsc::channel();
    let spawned = ::std::thread::Builder::new()
        .name("sagemaker-container-program".to_string())
        .stack_size(CONTAINER_PROGRAM_STACK_BYTES)
        .spawn(move || {
            // Held for as long as the run lasts, whoever is still waiting for it: a run this
            // caller has given up on keeps its slot until its thread really ends.
            let _slot = slot;
            let finished = python::run_against(
                &python::Run {
                    modules: &modules,
                    handler: &handler,
                    // "The environment variables to set in the Docker container": the job's own
                    // declared variables, and nothing of this process's.
                    environment: &variables,
                    // The instruction set the environment the program runs in has. A job names
                    // none, so it is the one the engine's own runtime reports when none is
                    // named.
                    architecture:
                        ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                    event: Some(&event),
                    request_id: "",
                    invoked_function_arn: "",
                    client_context: None,
                    tenant_id: None,
                    budget: python::Budget {
                        wall_clock: CONTAINER_PROGRAM_RUN_CEILING,
                        steps: MAX_CONTAINER_PROGRAM_STEPS,
                        memory_bytes: MAX_CONTAINER_PROGRAM_MEMORY_BYTES,
                        ephemeral_bytes: MAX_CONTAINER_PROGRAM_SCRATCH_BYTES,
                        log_bytes: MAX_CONTAINER_PROGRAM_LOG_BYTES,
                        response_bytes: MAX_CONTAINER_PROGRAM_ANSWER_BYTES,
                        recursion_limit: MAX_CONTAINER_PROGRAM_RECURSION,
                    },
                    // A job's container reaches no other service of this emulator from inside
                    // its program: the run is given no seam to dispatch an AWS call on, and the
                    // data it works over is what the service placed in its storage.
                    aws: None,
                    // It is no Lambda durable execution's run either, so it takes no step of one
                    // and checkpoints nothing onto one.
                    durable: None,
                },
                // The clock the run's own span is read against: one the RUN drives, a tick per
                // traced event it delivers, stating what the run may take of THIS host besides.
                &python::Clock::driven(CONTAINER_PROGRAM_CLOCK_TICK)
                    .within(CONTAINER_PROGRAM_HOST_CEILING),
            );
            let _ = sender.send(finished);
        });
    if spawned.is_err() {
        return ContainerOutcome::NotPerformed {
            reason: INTERNAL_FAILURE_REASON.to_string(),
        };
    }
    match receiver.recv_timeout(CONTAINER_PROGRAM_HOST_WAIT) {
        Ok(finished) => read_outcome(&finished.completion),
        // The thread has still not answered a grace period past the host time one run may take
        // — which the engine has by then stopped the run on — so this emulator gave up on it.
        Err(_) => ContainerOutcome::NotPerformed {
            reason: program_stopped("the host time one run of a container program may take"),
        },
    }
}

/// The EVENT one run is handed: the program to run, its command line, and the files the
/// container's storage holds before it runs. Every file travels as base64, so bytes that are
/// not text reach the program exactly as the service placed them.
fn run_event(
    interpreter: &str,
    form: &PythonProgram,
    files: &[(String, String)],
) -> Option<String> {
    let held: ::serde_json::Map<String, ::serde_json::Value> = files
        .iter()
        .map(|(path, contents)| {
            (
                path.clone(),
                ::serde_json::Value::String(::aws_smithy_types::base64::encode(contents.as_bytes())),
            )
        })
        .collect();
    let event = ::serde_json::json!({
        "interpreter": interpreter,
        "path": form.argv0(),
        "source": form.source(),
        "arguments": form.arguments(),
        "files": held,
        "streamCharacters": MAX_CONTAINER_PROGRAM_STREAM_CHARACTERS,
    });
    ::serde_json::to_string(&event).ok()
}

/// What the job reports for the run the interpreter answered for.
fn read_outcome(completion: &python::Completion) -> ContainerOutcome {
    match completion {
        // The entry module REPORTS what the program came to: the status it exited with, what it
        // wrote on its two console streams, and the files it left behind.
        python::Completion::Returned(answered) => reported_run(answered),
        // Nothing here asks for a streaming answer, and the entry module answers one value, so a
        // run that produced one is a run nothing can be read from.
        python::Completion::Streamed { .. } => ContainerOutcome::NotPerformed {
            reason: INTERNAL_FAILURE_REASON.to_string(),
        },
        // The ENTRY MODULE raised, which is not the program's own failure — it runs the program
        // inside its own guard and reports what it came to — so this is a fault of the engine
        // rather than a status the container produced.
        python::Completion::Raised { .. } | python::Completion::Internal(_) => {
            ContainerOutcome::NotPerformed {
                reason: INTERNAL_FAILURE_REASON.to_string(),
            }
        }
        python::Completion::Stopped(stop) => ContainerOutcome::NotPerformed {
            reason: program_stopped(match stop {
                python::Stop::Deadline => "the wall clock this emulator gives one program",
                python::Stop::Steps => "the step budget this emulator gives one program",
                python::Stop::Memory => "the memory ceiling this emulator gives one program",
                python::Stop::Response => {
                    "the size of what one program may leave in its container's storage"
                }
                python::Stop::HostCeiling => {
                    "the host time this emulator gives one run of a program"
                }
            }),
        },
    }
}

/// What the program came to, read out of the document the entry module reported it in.
///
/// A document this service cannot read is a run nothing is known about, which claims nothing.
fn reported_run(answered: &str) -> ContainerOutcome {
    let Ok(document) = ::serde_json::from_str::<::serde_json::Value>(answered) else {
        return ContainerOutcome::NotPerformed {
            reason: INTERNAL_FAILURE_REASON.to_string(),
        };
    };
    let Some(status) = document
        .get("exitStatus")
        .and_then(::serde_json::Value::as_i64)
        .and_then(|status| i32::try_from(status).ok())
    else {
        return ContainerOutcome::NotPerformed {
            reason: INTERNAL_FAILURE_REASON.to_string(),
        };
    };
    // The tail of what the program wrote on standard error, which is what AWS's
    // `AlgorithmError` form carries for a container that exited non-zero.
    let error_tail = document
        .get("stderr")
        .and_then(::serde_json::Value::as_str)
        .unwrap_or_default()
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .to_string();
    let files = document
        .get("files")
        .and_then(::serde_json::Value::as_object)
        .map(|held| {
            let mut files: Vec<(String, String)> = held
                .iter()
                .filter_map(|(path, encoded)| {
                    let encoded = encoded.as_str()?;
                    let bytes = ::aws_smithy_types::base64::decode(encoded).ok()?;
                    // The container's storage is read back as TEXT, which is what a job's own
                    // output stages carry ([`ContainerOutcome::Performed`]).
                    Some((path.clone(), String::from_utf8_lossy(&bytes).to_string()))
                })
                .collect();
            files.sort();
            files
        })
        .unwrap_or_default();
    ContainerOutcome::Performed {
        status,
        error_tail,
        files,
    }
}

/// The ENTRY MODULE one run resolves: it runs the declared program the way the interpreter runs
/// one — as the `__main__` module, with the interpreter's own `sys.argv` — over the container's
/// own storage, and reports what the run came to.
///
/// Ordinary Python, the way every module this workspace supplies an interpreter is. It is NOT
/// the containment — that is the engine's, in host code no program can reach: no host path
/// resolves in the interpreter this runs in, with or without this module. What it is is the
/// container's own FILESYSTEM: the tree the service placed the job's data in, which the program
/// reads through `open` and the directory readings of `os`, and whose files are read back
/// afterwards as the job's output.
const CONTAINER_KERNEL_SOURCE: &str = r#"
"""Runs the program a SageMaker job's container declares, over the container's own storage."""

import base64
import builtins
import io
import os
import posixpath
import sys
import traceback

try:
    import fnmatch
except ImportError:
    fnmatch = None
try:
    import glob
except ImportError:
    glob = None

# The directory a container's program starts in, which a relative path resolves against.
_WORKING_DIRECTORY = "/"
# What the interpreter writes and exits with when the file it was given to run is not there.
_NO_SUCH_FILE_STATUS = 2


class _Console:
    """One console stream of the program, bounded by what one run keeps of it."""

    def __init__(self, limit):
        self._parts = []
        self._kept = 0
        self._limit = limit

    def write(self, text):
        if not isinstance(text, str):
            text = str(text)
        room = self._limit - self._kept
        if room > 0:
            kept = text[:room]
            self._parts.append(kept)
            self._kept += len(kept)
        return len(text)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        return None

    def isatty(self):
        return False

    def text(self):
        return "".join(self._parts)


def _path_of(named, cwd):
    """The absolute path `named` means, as the container's own filesystem reads one."""
    if isinstance(named, bytes):
        named = named.decode("utf-8", "surrogateescape")
    elif not isinstance(named, str):
        held = getattr(named, "__fspath__", None)
        if held is None:
            raise TypeError(
                "expected str, bytes or os.PathLike, not %s" % (type(named).__name__,)
            )
        return _path_of(held(), cwd)
    if not named.startswith("/"):
        named = posixpath.join(cwd, named)
    return posixpath.normpath(named)


class _Storage(object):
    """The container's own storage: the files the service placed in it, and the ones the
    program writes. Held in this run's own memory and nowhere else."""

    def __init__(self, files, cwd):
        self._files = {}
        self._directories = set(["/"])
        self._cwd = cwd
        for path in files:
            self.put(_path_of(path, cwd), files[path])
        self._hold(cwd)

    def _hold(self, directory):
        while directory and directory != "/":
            self._directories.add(directory)
            directory = posixpath.dirname(directory)
        self._directories.add("/")

    def put(self, path, data):
        self._files[path] = bytes(data)
        self._hold(posixpath.dirname(path))

    def path(self, named):
        return _path_of(named, self._cwd)

    def cwd(self):
        return self._cwd

    def read(self, named):
        path = self.path(named)
        if path in self._files:
            return self._files[path]
        if path in self._directories:
            raise IsADirectoryError(21, "Is a directory", path)
        raise FileNotFoundError(2, "No such file or directory", path)

    def held(self, path):
        return self._files.get(path, b"")

    def exists(self, named):
        path = self.path(named)
        return path in self._files or path in self._directories

    def isfile(self, named):
        return self.path(named) in self._files

    def isdir(self, named):
        return self.path(named) in self._directories

    def size(self, named):
        return len(self.read(named))

    def names(self, named):
        path = self.path(named)
        if path not in self._directories:
            raise FileNotFoundError(2, "No such file or directory", path)
        prefix = path.rstrip("/") + "/"
        children = set()
        for held in list(self._files) + list(self._directories):
            if held.startswith(prefix):
                children.add(held[len(prefix):].split("/")[0])
        return sorted(children)

    def makedirs(self, named, mode=0o777, exist_ok=False):
        path = self.path(named)
        if path in self._files:
            raise FileExistsError(17, "File exists", path)
        if path in self._directories and not exist_ok:
            raise FileExistsError(17, "File exists", path)
        self._hold(path)

    def mkdir(self, named, mode=0o777, **kept):
        self.makedirs(named, mode, False)

    def remove(self, named):
        path = self.path(named)
        if path not in self._files:
            raise FileNotFoundError(2, "No such file or directory", path)
        del self._files[path]

    def rename(self, source, target):
        held = self.read(source)
        self.put(self.path(target), held)
        del self._files[self.path(source)]

    def walk(self, named, topdown=True, onerror=None, followlinks=False):
        root = self.path(named)
        if root not in self._directories:
            return
        pending = [root]
        while pending:
            directory = pending.pop(0)
            directories = []
            files = []
            for name in self.names(directory):
                child = posixpath.join(directory, name)
                if child in self._directories:
                    directories.append(name)
                else:
                    files.append(name)
            yield (directory, directories, files)
            for name in directories:
                pending.append(posixpath.join(directory, name))

    def matching(self, pattern):
        """Every path of the tree one shell pattern names, matched segment by segment — so a
        wildcard stands for part of ONE name and never for a directory boundary."""
        if fnmatch is None:
            return []
        wanted = [part for part in self.path(pattern).split("/") if part]
        held = sorted(
            list(self._files) + [entry for entry in self._directories if entry != "/"]
        )
        matched = []
        for entry in held:
            parts = [part for part in entry.split("/") if part]
            if len(parts) != len(wanted):
                continue
            if all(
                fnmatch.fnmatchcase(part, segment)
                for part, segment in zip(parts, wanted)
            ):
                matched.append(entry)
        return matched

    def entries(self):
        return dict(self._files)


class _Writer(object):
    """A file of the container's storage open for writing: what it writes lands in the storage
    as it is written, so a program that never closes its file still leaves what it wrote."""

    def __init__(self, storage, path, binary, encoding, errors, append):
        self._storage = storage
        self._path = path
        self._binary = binary
        self._encoding = encoding or "utf-8"
        self._errors = errors or "strict"
        self._held = bytearray(storage.held(path) if append else b"")
        self.name = path
        self.closed = False
        storage.put(path, bytes(self._held))

    def write(self, text):
        if self._binary:
            chunk = bytes(text)
        else:
            if not isinstance(text, str):
                raise TypeError("write() argument must be str")
            chunk = text.encode(self._encoding, self._errors)
        self._held.extend(chunk)
        self._storage.put(self._path, bytes(self._held))
        return len(text)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        return None

    def close(self):
        self.closed = True

    def readable(self):
        return False

    def writable(self):
        return True

    def seekable(self):
        return False

    def __iter__(self):
        raise io.UnsupportedOperation("not readable")

    def __enter__(self):
        return self

    def __exit__(self, kind, value, trace):
        self.close()
        return False


def _opener(storage):
    """`open` over the container's storage: the only filesystem the program sees."""

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
        path = storage.path(file)
        binary = "b" in mode
        writing = any(flag in mode for flag in ("w", "a", "x", "+"))
        if writing:
            if "x" in mode and storage.isfile(path):
                raise FileExistsError(17, "File exists", path)
            return _Writer(storage, path, binary, encoding, errors, "a" in mode)
        held = storage.read(path)
        if binary:
            return io.BytesIO(held)
        return io.StringIO(held.decode(encoding or "utf-8", errors or "strict"))

    return open_file


def _install(storage):
    """Serve the container's storage on the names a program reads its filesystem through.

    The host-reaching calls those names were built on are already gone — the engine took every
    way of opening a file of the HOST away before this module ran — so what this does is give
    the program the container's own files, not reach anything new.
    """
    builtins.open = _opener(storage)
    io.open = builtins.open
    os.listdir = storage.names
    os.makedirs = storage.makedirs
    os.mkdir = storage.mkdir
    os.remove = storage.remove
    os.unlink = storage.remove
    os.rename = storage.rename
    os.replace = storage.rename
    os.walk = storage.walk
    os.getcwd = storage.cwd
    os.path.exists = storage.exists
    os.path.lexists = storage.exists
    os.path.isfile = storage.isfile
    os.path.isdir = storage.isdir
    os.path.getsize = storage.size
    if glob is not None:
        glob.glob = lambda pattern, **kept: storage.matching(pattern)
        glob.iglob = lambda pattern, **kept: iter(storage.matching(pattern))


def _files_of(event):
    """The container's storage as the run was handed it, keyed by its own paths."""
    held = {}
    for path in event.get("files") or {}:
        held[path] = base64.b64decode(event["files"][path])
    return held


def run(event, context):
    storage = _Storage(_files_of(event), _WORKING_DIRECTORY)
    _install(storage)
    limit = event["streamCharacters"]
    out = _Console(limit)
    err = _Console(limit)
    kept = (sys.stdout, sys.stderr)
    sys.stdout = out
    sys.stderr = err
    status = 0
    error_type = ""
    error_message = ""
    path = event["path"]
    try:
        # The command line the program resolves its arguments off: the program's own name
        # followed by the arguments the vector carried, which is what the interpreter hands it.
        sys.argv = [path] + list(event.get("arguments") or [])
        source = event.get("source")
        if source is None:
            # A script file, read out of the container's storage. A file that is not there is
            # what the interpreter itself reports and exits on, before any program runs.
            if not storage.isfile(path):
                status = _NO_SUCH_FILE_STATUS
                err.write(
                    "%s: can't open file '%s': [Errno 2] No such file or directory\n"
                    % (event.get("interpreter") or "python3", path)
                )
                raise SystemExit(status)
            source = storage.read(path).decode("utf-8", "surrogateescape")
        globals_of_main = {"__name__": "__main__", "__file__": path}
        exec(compile(source, path, "exec"), globals_of_main)
    except SystemExit as ended:
        # sys.exit() is how a program names the status it exits with: no argument and None are a
        # clean exit, an integer is that status — masked to the eight bits a process exit
        # carries — and anything else is printed on standard error and exits non-zero, exactly
        # as the interpreter does with it.
        code = ended.code
        if code is None or code is False:
            status = 0
        elif code is True:
            status = 1
        elif isinstance(code, int):
            status = int(code) & 0xFF
        else:
            status = 1
            err.write("%s\n" % (code,))
    except BaseException as raised:
        # The interpreter writes the program's traceback on standard error and exits non-zero.
        status = 1
        error_type = type(raised).__name__
        error_message = str(raised)
        trace = sys.exc_info()[2]
        # This module's own frame is not part of the program's traceback.
        if trace is not None and trace.tb_next is not None:
            trace = trace.tb_next
        err.write("".join(traceback.format_exception(type(raised), raised, trace)))
    finally:
        sys.stdout, sys.stderr = kept
    files = {}
    entries = storage.entries()
    for held in entries:
        files[held] = base64.b64encode(entries[held]).decode("ascii")
    return {
        "exitStatus": status,
        "stdout": out.text(),
        "stderr": err.text(),
        "errorType": error_type,
        "errorMessage": error_message,
        "files": files,
    }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a container entrypoint names the Python interpreter by the names a Python 3
    /// environment provides for it, by bare name or by path — and nothing else.
    ///
    /// The rows differ along the FORM of the name: bare, absolute, version-qualified, the
    /// unqualified name, an interpreter of another major version, and a program that is no
    /// interpreter at all.
    #[test]
    fn the_interpreter_is_named_by_the_names_a_python_environment_provides() {
        // (case, the vector's first word, the name it names the interpreter by)
        let rows: [(&str, &str, Option<&str>); 8] = [
            ("the bare name", "python3", Some("python3")),
            ("an absolute path", "/usr/bin/python3", Some("python3")),
            ("a version-qualified name", "python3.11", Some("python3.11")),
            ("a version-qualified path", "/usr/local/bin/python3.9", Some("python3.9")),
            ("the unqualified name", "python", Some("python")),
            ("another major version", "python2", None),
            ("a shell", "/bin/sh", None),
            ("a script of the caller's own", "/opt/ml/code/train.py", None),
        ];
        for (case, word, expected) in rows {
            let program = vec![word.to_string()];
            let read = python_interpreter(&program);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the program an interpreter vector names is the `-c` source or the script path,
    /// with the words that follow it as its arguments, and the interpreter's own options passed
    /// over; a vector naming no program at all names none.
    #[test]
    fn the_vector_names_the_source_or_the_script_it_runs() {
        // (case, the vector, the program it names)
        let rows: [(&str, &[&str], Option<PythonProgram>); 6] = [
            (
                "a command given on the command line",
                &["python3", "-c", "print(1)"],
                Some(PythonProgram::Command {
                    source: "print(1)".to_string(),
                    arguments: Vec::new(),
                }),
            ),
            (
                "a command with arguments of its own",
                &["python3", "-c", "import sys", "first", "second"],
                Some(PythonProgram::Command {
                    source: "import sys".to_string(),
                    arguments: vec!["first".to_string(), "second".to_string()],
                }),
            ),
            (
                "an option before the command",
                &["python3", "-u", "-c", "print(1)"],
                Some(PythonProgram::Command {
                    source: "print(1)".to_string(),
                    arguments: Vec::new(),
                }),
            ),
            (
                "a script file",
                &["python3", "/opt/ml/code/train.py"],
                Some(PythonProgram::Script {
                    path: "/opt/ml/code/train.py".to_string(),
                    arguments: Vec::new(),
                }),
            ),
            (
                "a script file with arguments",
                &["python3", "train.py", "--epochs", "3"],
                Some(PythonProgram::Script {
                    path: "train.py".to_string(),
                    arguments: vec!["--epochs".to_string(), "3".to_string()],
                }),
            ),
            ("the interpreter alone", &["python3"], None),
        ];
        for (case, words, expected) in rows {
            let program: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
            let read = python_program(&program);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the status a Python program exits with is the status the run reports, and the
    /// files it left in the container's storage are the ones read back out of the run — over
    /// the data the service placed there before the program started.
    ///
    /// The rows differ along how the program ENDS: running to its end, exiting a status of its
    /// own, and raising; and along what it reads: a seeded input file and the job's own
    /// environment.
    #[test]
    fn a_python_program_runs_over_the_containers_own_storage() {
        /// One row: the case, the source the vector carries, the status it exits with, and the
        /// file it has to leave with its contents.
        struct Row {
            case: &'static str,
            source: &'static str,
            status: i32,
            left: Option<(&'static str, &'static str)>,
        }
        let rows = [
            Row {
                case: "a program that reads its input and writes the model directory",
                source: "import os\n\
                         rows = open('/opt/ml/input/data/train/data.csv').read().splitlines()\n\
                         os.makedirs('/opt/ml/model', exist_ok=True)\n\
                         open('/opt/ml/model/artifact.txt', 'w').write('%d rows\\n' % len(rows))\n",
                status: 0,
                left: Some(("/opt/ml/model/artifact.txt", "2 rows\n")),
            },
            Row {
                case: "a program that exits a status of its own",
                source: "import sys\nsys.exit(7)\n",
                status: 7,
                left: None,
            },
            Row {
                case: "a program that raises",
                source: "raise ValueError('no data')\n",
                status: 1,
                left: None,
            },
            Row {
                case: "a program that lists the directory its channel was placed in",
                source: "import os\n\
                         names = sorted(os.listdir('/opt/ml/input/data/train'))\n\
                         os.makedirs('/opt/ml/model', exist_ok=True)\n\
                         open('/opt/ml/model/names.txt', 'w').write(','.join(names))\n",
                status: 0,
                left: Some(("/opt/ml/model/names.txt", "data.csv")),
            },
        ];
        let inputs = vec![(
            "/opt/ml/input/data/train/data.csv".to_string(),
            "1,2\n3,4\n".to_string(),
        )];
        for row in rows {
            let case = row.case;
            let program: Vec<String> = ["python3", "-c", row.source]
                .iter()
                .map(|word| (*word).to_string())
                .collect();
            let outcome = run_python_container_program(&program, None, &inputs);
            let ContainerOutcome::Performed {
                status,
                error_tail,
                files,
            } = &outcome
            else {
                panic!("case {case:?}: expected the program to be performed, got {outcome:?}");
            };
            assert_eq!(
                *status, row.status,
                "case {case:?}: expected exit status {}, got {status} with {error_tail:?}",
                row.status
            );
            match row.left {
                Some((path, contents)) => {
                    let held = outcome.file(path);
                    assert_eq!(
                        held,
                        Some(contents),
                        "case {case:?}: expected {path} to hold {contents:?}, got {held:?}"
                    );
                }
                None => assert!(
                    !files.iter().any(|(path, _)| path.starts_with("/opt/ml/model")),
                    "case {case:?}: a program that wrote nothing leaves nothing, got {files:?}"
                ),
            }
            assert_eq!(
                outcome.failure_reason().is_some(),
                row.status != 0,
                "case {case:?}: only a non-zero status fails the job, got {outcome:?}"
            );
        }
    }

    /// RULE: a program this emulator started but could not finish claims no exit status: the
    /// reason names the bound it was stopped on rather than the `AlgorithmError` form, which
    /// belongs to a container that really exited.
    #[test]
    fn a_program_stopped_on_a_bound_claims_no_exit_status() {
        let program: Vec<String> = ["python3", "-c", "while True:\n    pass\n"]
            .iter()
            .map(|word| (*word).to_string())
            .collect();
        let outcome = run_python_container_program(&program, None, &[]);
        let ContainerOutcome::NotPerformed { reason } = &outcome else {
            panic!("expected a program stopped on a bound, got {outcome:?}");
        };
        assert!(
            !reason.contains("AlgorithmError") && !reason.contains("exit code"),
            "nothing exited, so no exit status may be claimed, got {reason:?}"
        );
    }

    /// RULE: the only filesystem a container program sees is the container's own storage — the
    /// files the service placed there and the ones the program wrote — so a path of the HOST
    /// resolves to nothing, and starting a process is refused.
    ///
    /// The containment itself is the ENGINE's, in host code no program can reach; this pins
    /// that a program run through this module inherits it rather than being handed a way
    /// around it.
    #[test]
    fn a_program_reaches_no_host_file_and_starts_no_process() {
        let source = "import os\n\
             seen = []\n\
             try:\n\
             \x20   open('/etc/passwd').read()\n\
             \x20   seen.append('read-the-host')\n\
             except Exception:\n\
             \x20   seen.append('no-host-file')\n\
             try:\n\
             \x20   import subprocess\n\
             \x20   subprocess.run(['/bin/sh', '-c', 'true'])\n\
             \x20   seen.append('started-a-process')\n\
             except Exception:\n\
             \x20   seen.append('no-process')\n\
             os.makedirs('/opt/ml/model', exist_ok=True)\n\
             open('/opt/ml/model/probe.txt', 'w').write(','.join(seen))\n";
        let program: Vec<String> = ["python3", "-c", source]
            .iter()
            .map(|word| (*word).to_string())
            .collect();
        let outcome = run_python_container_program(&program, None, &[]);
        assert_eq!(
            outcome.failure_reason(),
            None,
            "the probe program itself runs to its end, got {outcome:?}"
        );
        let held = outcome.file("/opt/ml/model/probe.txt");
        assert_eq!(
            held,
            Some("no-host-file,no-process"),
            "expected the host file and the process to be refused, got {held:?}"
        );
    }

    /// RULE: the environment the job declares is the environment the program reads out of
    /// `os.environ`, so a program branching on one of its variables answers out of what the
    /// request carried.
    #[test]
    fn the_program_sees_the_jobs_own_environment() {
        let mut environment = HashMap::new();
        environment.insert("MODE".to_string(), "fail".to_string());
        let program: Vec<String> = [
            "python3",
            "-c",
            "import os, sys\nsys.exit(4 if os.environ.get('MODE') == 'fail' else 0)\n",
        ]
        .iter()
        .map(|word| (*word).to_string())
        .collect();
        let outcome = run_python_container_program(&program, Some(&environment), &[]);
        assert_eq!(
            outcome.failure_reason(),
            Some(algorithm_error("", 4)),
            "expected the status the environment selected, got {outcome:?}"
        );
    }
}
