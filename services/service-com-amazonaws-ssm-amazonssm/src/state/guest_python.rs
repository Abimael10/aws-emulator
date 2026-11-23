// SPDX-License-Identifier: CC0-1.0

//! The PYTHON INTERPRETER in the guest of a managed node: `python3`, RUNNING the program a
//! command's script hands it.
//!
//! # Why a script needs one
//!
//! The universal idiom for reading in-guest state through Run Command is a script that ends in
//! an inline Python program and prints one line for the client to parse:
//!
//! ```text
//! python3 - <<'EOF'
//! import json, os
//! print(json.dumps({"hits": os.environ.get("HITS", "")}))
//! EOF
//! ```
//!
//! What `GetCommandInvocation` reports as `StandardOutputContent` is what the invoked commands
//! wrote to standard output, so for such a script that stream carries the PROGRAM's own output
//! — a line the program computed — and nothing else. An interpreter the guest does not have
//! would leave a client parsing a diagnostic instead of its program's answer, which is no
//! answer at all.
//!
//! # Why it is installed from here
//!
//! The one contained Python engine this workspace embeds is the Lambda service crate's
//! ([`::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python`]), the same
//! engine an `aws:executeScript` automation step runs its script in
//! ([`super::automation::actions::script_step`]). The service that owns the GUEST cannot reach
//! it, so this crate installs the interpreter into the command's session beside the machine's
//! own programs ([`super::guest_execution::perform_in_guest`]) — the way the resolver clients
//! ([`super::guest_resolver`]) and the package-query client ([`super::guest_packages`]) are
//! installed — rather than a second execution substrate being stood up for the guest.
//!
//! Everything that CONTAINS and BOUNDS a run therefore lives in that one engine and is
//! maintained once: its native modules are an allow-list with no socket, subprocess,
//! memory-map, native-library or thread module in it, its `posix` is that crate's own with
//! every host-reaching call refused, every way of opening a HOST file is taken away before any
//! guest code runs, and the deadline, traced-step, memory, recursion and output bounds are
//! enforced in host code the program cannot reach — including the latch that makes a stopped
//! run's answer that stop whatever the program catches.
//!
//! # What this module adds
//!
//! * the INVOCATION read off the command's words ([`Invocation`]): the program on `-c`, the
//!   one on standard input (`-` or a here-document), a file of the machine's own storage, or
//!   the module `-m` names — the forms a script writes an interpreter in;
//! * the ENVIRONMENT the program runs with, which is the variables the session holds, handed
//!   to the engine so they are installed by the HOST before a line of the program is read —
//!   a value an earlier line of the script exported is what `os.environ` answers;
//! * the KERNEL ([`KERNEL_SOURCE`]) — ordinary Python, handed to the engine as the module the
//!   run resolves — which executes the program as `__main__` with its two output streams kept
//!   APART, so what the program wrote to standard output is the command's output and its
//!   traceback is the command's standard error, beside the status the program ended on;
//! * the BOUNDS one program and one REQUEST's programs are held to: the run's own budget and
//!   clock, one of this process's run places ([`RUN_PLACES`]), the allowance every program of
//!   one request spends from ([`ProgramAllowance`]), and the bounded wait the command gives the
//!   thread the run happens on ([`HOST_WAIT`]) — so what a request can make this process run is
//!   bounded whatever its script, its node selection and its arrival rate come to.
//!
//! Nothing here starts a process, opens a host file or reaches a socket, and nothing is
//! reported for a program that did not run.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    Budget, Clock, Completion, Stop, RUN_STACK_BYTES,
};
use ::service_common::posix_shell_session::{
    EnvironmentPrograms, ProgramFiles, ProgramInvocation, ProgramOutcome, ProgramStreams,
};

/// The names the interpreter is invoked under, which is the ONE catalog of them this
/// workspace reads ([`::service_common::static_web_servers::INTERPRETERS`], where the same two
/// names are read for the server module one of them serves a directory with).
use ::service_common::static_web_servers::INTERPRETERS;

/// The module the kernel is handed to the engine as, and the entry point it resolves on it.
const KERNEL_MODULE: &str = "node_guest_python";

/// See [`KERNEL_MODULE`].
const KERNEL_HANDLER: &str = "node_guest_python.main";

/// The status a program that ran to its end without ending itself exits with.
const SUCCESS: i32 = 0;

/// The status the interpreter exits with for a program that ended on an uncaught exception,
/// which is what CPython exits with for one.
const RAISED: i32 = 1;

/// The status the interpreter exits with when it could not even read the program it was given:
/// a file it cannot open, and an option it does not perform — CPython's own usage failure.
const USAGE_FAILURE: i32 = 2;

/// How long ONE guest program may run in the RUN'S OWN time.
///
/// CHOSEN, not an AWS duration: the program runs in this process, inside the call that
/// performs the command, so this and the bounds beside it are what one caller-supplied program
/// can cost. It is a span of the RUN's own time rather than of this host's — read against a
/// clock the run itself drives ([`CLOCK_TICK`]) and spent by the waits the program asks for —
/// so what a program comes to is the same on an idle host and on a loaded one. What the run may
/// take of THIS host is bounded beside it ([`HOST_CEILING`]), and what every program of one
/// REQUEST may take between them is bounded above both ([`ProgramAllowance`]).
const RUN_CEILING: Duration = Duration::from_secs(5);

/// How far ONE reading of the run's clock moves it, sized so the whole step budget
/// ([`MAX_STEPS`]) spends well under [`RUN_CEILING`] — which leaves a program that computes
/// without end to be answered by the step budget, a count of its OWN work.
const CLOCK_TICK: Duration = Duration::from_nanos(100);

/// How much of THIS HOST's own time one run may take, past which the engine stops it whatever
/// its own clock reads.
///
/// CHOSEN: the span above bounds no CPU of this host — a program spending traced events, or
/// sitting in one long native call that delivers none, would hold a thread of this process for
/// as long as the host took over it. This is the bound that stops it, read from this host's own
/// clock at every traced event and by the engine's own watch over the run. It allows for the
/// engine's two phases ONCE each (standing the interpreter up, then the program itself),
/// because a program that FINISHED must be reported on what it came to rather than on this
/// host's load.
const HOST_CEILING: Duration =
    ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::INIT_PHASE_LIMIT
        .saturating_add(RUN_CEILING);

/// How long the command that invoked the interpreter waits for the thread the run happens on,
/// at most: the host ceiling above plus the grace the engine's own stop needs to land, since
/// that stop is latched by the host and RAISED at the next traced event.
///
/// Past it the run has been GIVEN UP on: the command is answered rather than left waiting, the
/// thread keeps its place in the pool until it really ends ([`RUN_PERMITS`]), and the span it
/// spent is charged to the request's allowance all the same.
const HOST_WAIT: Duration = HOST_CEILING.saturating_add(Duration::from_secs(3));

/// How many guest programs this process runs AT ONCE.
///
/// CHOSEN: a program is a caller's text and each run stands an interpreter up on a thread of
/// its own, so without this the memory and CPU of one run would be multiplied by however many
/// requests arrive at once. A command that finds every place taken waits a bounded span
/// ([`PLACE_WAIT`]) and then reports a program it did not run, rather than queueing behind
/// another request's for as long as that one lasts.
const MAX_CONCURRENT_PROGRAMS: usize = 4;

/// How long a command waits for one of [`MAX_CONCURRENT_PROGRAMS`] places, at most.
///
/// CHOSEN, the same span this crate's automation scripts wait for an interpreter
/// ([`super::automation::actions::SCRIPT_PERMIT_WAIT`]): the wait is bounded, so the command
/// is answered either way, and a program that waited the whole wait has not been run at all.
const PLACE_WAIT: Duration = Duration::from_secs(5);

/// The places [`MAX_CONCURRENT_PROGRAMS`] hands out, for the whole process.
///
/// A SYNCHRONOUS pool, because the session a guest program runs in is driven synchronously
/// (`…ec2…::state::guest_commands`): a count of free places and the condition that a place was
/// given back. A place is MOVED INTO the thread the run happens on, so it is given back when
/// that thread really ends — including a run the command has already been given up on, which
/// would otherwise take this bound past its count while its thread was still going.
static RUN_PLACES: ::std::sync::LazyLock<RunPool> = ::std::sync::LazyLock::new(|| RunPool {
    free: ::std::sync::Mutex::new(MAX_CONCURRENT_PROGRAMS),
    given_back: ::std::sync::Condvar::new(),
});

/// The pool of places the runs of this process take ([`RUN_PLACES`]).
#[derive(Debug)]
struct RunPool {
    /// How many places are free.
    free: ::std::sync::Mutex<usize>,
    /// Signalled whenever a run gives its place back.
    given_back: ::std::sync::Condvar,
}

impl RunPool {
    /// Take one place, waiting at most [`PLACE_WAIT`] for one — `None` when every place was
    /// still taken by then, which is a program this emulator did not run.
    fn taken(&'static self) -> Option<RunPlace> {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner);
        let deadline = ::std::time::Instant::now() + PLACE_WAIT;
        while *free == 0 {
            let left = deadline.saturating_duration_since(::std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            let (held, waited) = self
                .given_back
                .wait_timeout(free, left)
                .unwrap_or_else(::std::sync::PoisonError::into_inner);
            free = held;
            if waited.timed_out() && *free == 0 {
                return None;
            }
        }
        *free = free.saturating_sub(1);
        Some(RunPlace { pool: self })
    }

    /// Give one place back and tell whoever is waiting for one.
    fn give_back(&self) {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner);
        *free = free.saturating_add(1);
        drop(free);
        self.given_back.notify_one();
    }
}

/// One place of the pool, given back when it is DROPPED — on the thread the run happened on,
/// which is what keeps a run this emulator gave up on counted until it really ends.
#[derive(Debug)]
struct RunPlace {
    pool: &'static RunPool,
}

impl Drop for RunPlace {
    fn drop(&mut self) {
        self.pool.give_back();
    }
}

/// How many guest programs ONE REQUEST runs, over every command of every script it delivers.
///
/// CHOSEN: the request chooses how many commands its script carries and how many nodes it
/// delivers to, so each of those factors is bounded where it is read
/// (`…ec2…::state::guest_commands::MAX_GUEST_RUN_COMMANDS`,
/// [`super::commands::MAX_GUEST_DELIVERIES_PER_SEND`]) and their PRODUCT — how many programs
/// one request can make this process run — is bounded here, by an allowance every program of
/// that request spends from ([`ProgramAllowance`]).
const MAX_PROGRAMS_PER_REQUEST: usize = 16;

/// How much of THIS HOST's own time the programs of one request may take between them, which
/// is the same product bounded in the other direction: a request whose programs have spent it
/// runs no further program, however many of its allowance is left.
const MAX_HOST_TIME_PER_REQUEST: Duration = Duration::from_secs(30);

/// What the programs of ONE REQUEST may spend between them: how many runs are left, and how
/// much of this host's own time.
///
/// One of these is made per request and shared by every guest session that request performs
/// ([`super::guest_execution::perform_in_guest`]), so the bound holds across the commands of
/// one script AND across the deliveries of one send rather than per invocation of the
/// interpreter. Spending is recorded by the host, in code no program can reach.
#[derive(Debug)]
pub struct ProgramAllowance {
    /// How many runs are left of [`MAX_PROGRAMS_PER_REQUEST`].
    runs_left: AtomicUsize,
    /// How many milliseconds of this host's own time are left of
    /// [`MAX_HOST_TIME_PER_REQUEST`].
    host_millis_left: AtomicU64,
}

impl ProgramAllowance {
    /// The allowance ONE request's programs share.
    #[must_use]
    pub fn for_one_request() -> Arc<Self> {
        Arc::new(Self {
            runs_left: AtomicUsize::new(MAX_PROGRAMS_PER_REQUEST),
            host_millis_left: AtomicU64::new(
                u64::try_from(MAX_HOST_TIME_PER_REQUEST.as_millis()).unwrap_or(u64::MAX),
            ),
        })
    }

    /// Take ONE run out of this allowance, answering whether there was one to take: `false` is
    /// a request that has already run the programs it may, which runs no further one.
    fn taken(&self) -> bool {
        if self.host_millis_left.load(Ordering::Relaxed) == 0 {
            return false;
        }
        self.runs_left
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_ok()
    }

    /// Charge `elapsed` of this host's own time to the allowance, which is what the run just
    /// performed took of it.
    fn spent(&self, elapsed: Duration) {
        let took = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let _ = self
            .host_millis_left
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                Some(left.saturating_sub(took))
            });
    }
}

/// How many traced events one program may execute.
///
/// CHOSEN: large enough for any value a script computes in an inline program — the frozen
/// standard library's own import chain spends a large share of it before the program's first
/// line — and far below what the span the run's clock gives it allows for, so a program that
/// loops without end is answered by THIS budget rather than by a clock.
const MAX_STEPS: u64 = 2_000_000;

/// How much memory one program may add to this process. CHOSEN, for the same reason.
const MAX_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// How much one program may write to the scratch directory of the environment it runs in.
/// CHOSEN, for the same reason: that directory is held in this process's memory.
const MAX_SCRATCH_BYTES: u64 = 64 * 1024 * 1024;

/// How deep one program may recurse. The interpreter's own default is below what the frozen
/// standard library's import chain needs, so this is the limit a run is given; the call that
/// would raise it is taken away by the engine before any program code runs.
const MAX_RECURSION: usize = 2_000;

/// How much of what one program WROTE this emulator holds while the run is in flight.
const MAX_LOG_BYTES: usize = 64 * 1024;

/// How large the answer one run may hand back is: the two streams the kernel reports and the
/// status, so this is what bounds what one program can make this process hold of its output.
const MAX_ANSWER_BYTES: usize = 256 * 1024;

/// What one guest program run may spend.
fn run_budget() -> Budget {
    Budget {
        wall_clock: RUN_CEILING,
        steps: MAX_STEPS,
        memory_bytes: MAX_MEMORY_BYTES,
        ephemeral_bytes: MAX_SCRATCH_BYTES,
        log_bytes: MAX_LOG_BYTES,
        response_bytes: MAX_ANSWER_BYTES,
        recursion_limit: MAX_RECURSION,
    }
}

/// The interpreter a command's script runs its inline programs with.
///
/// Holds the allowance the programs of THIS REQUEST spend from and nothing else: the engine is
/// the process's, and everything one run is given — the program, its arguments and its
/// environment — comes from the invocation itself. No store guard, no engine handle and no
/// host handle is reachable from a program.
#[derive(Debug)]
pub struct GuestInterpreter {
    /// What every program of this request may spend between them ([`ProgramAllowance`]).
    allowance: Arc<ProgramAllowance>,
}

/// The Python interpreter for the guest of a managed node, as the programs its environment
/// provides — spending from `allowance`, which is the REQUEST's and is shared by every guest
/// session that request performs.
#[must_use]
pub fn interpreter_programs(allowance: &Arc<ProgramAllowance>) -> Box<dyn EnvironmentPrograms> {
    Box::new(GuestInterpreter {
        allowance: Arc::clone(allowance),
    })
}

impl EnvironmentPrograms for GuestInterpreter {
    /// The invocation performed for a caller that asks this form: with no environment, because
    /// this form carries none, and reporting what the program wrote to standard output alone.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let invocation = ProgramInvocation {
            name,
            arguments,
            standard_input: None,
            working_directory: "",
            environment: &BTreeMap::new(),
        };
        self.run_invocation(&invocation, files)
            .map(|streams| ProgramOutcome {
                status: streams.status,
                output: streams.output,
            })
    }

    fn run_invocation(
        &self,
        invocation: &ProgramInvocation<'_>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        // A program named by its absolute path is the same program, as a shell resolving a
        // name on `PATH` finds.
        let name = invocation
            .name
            .rsplit('/')
            .next()
            .unwrap_or(invocation.name);
        if !INTERPRETERS.contains(&name) {
            return None;
        }
        let read = Invocation::read(invocation.arguments, invocation.standard_input, files);
        Some(match read {
            Invocation::Usage { message, status } => ProgramStreams {
                status,
                output: Vec::new(),
                errors: vec![format!("{name}: {message}")],
            },
            // A stream with nothing on it is a program with nothing in it, which is what an
            // interpreter reading its program from one that is at its end runs.
            Invocation::Nothing => ProgramStreams {
                status: SUCCESS,
                ..ProgramStreams::default()
            },
            Invocation::Program { event } => {
                performed(name, &event, invocation.environment, &self.allowance)
            }
        })
    }
}

/// What a command's words ask the interpreter to run.
#[derive(Debug)]
enum Invocation {
    /// A program to run, as the event the kernel is handed.
    Program {
        /// The event document: the source or the module, the file name its frames carry, and
        /// the arguments the program reads as `sys.argv`.
        event: ::serde_json::Value,
    },
    /// Nothing to run at all.
    Nothing,
    /// The interpreter's own usage failure: what it reports and the status it exits with.
    Usage { message: String, status: i32 },
}

/// The options the interpreter takes a VALUE for in the next word, each of which changes
/// nothing a program of this guest observes: the warning filter, the implementation option, and
/// where the interpreter would start a check from.
const VALUED_OPTIONS: &[&str] = &["-W", "-X", "--check-hash-based-pycs"];

/// The flag options this interpreter performs by IGNORING them: each one changes how the
/// interpreter itself is set up — unbuffered streams, which paths are on the import path,
/// whether bytecode is written, how loudly it reports — and none of them changes what a
/// program computes or writes, which is what a command reads back.
const IGNORED_OPTIONS: &[&str] = &[
    "-b", "-B", "-d", "-E", "-I", "-O", "-OO", "-P", "-q", "-s", "-S", "-u", "-v", "-x",
];

/// The option that names the program on the command line itself.
const COMMAND_OPTION: &str = "-c";

/// The option that names a MODULE to run as `__main__`.
const MODULE_OPTION: &str = "-m";

/// The operand that names standard input as where the program is read from.
const STANDARD_INPUT_OPERAND: &str = "-";

/// The file name the frames of a program read from standard input carry, which is what CPython
/// names it.
const STANDARD_INPUT_FILE: &str = "<stdin>";

/// The file name the frames of a program given on the command line carry.
const COMMAND_FILE: &str = "<string>";

/// How large a program this interpreter reads at all.
///
/// The program is a caller's text — the script's own here-document, or a file of the machine's
/// storage — so what one invocation can hand the engine is bounded here. It is the size one
/// line of a command document can carry in full.
const MAX_PROGRAM_BYTES: usize = 256 * 1024;

/// How many words one program reads as its arguments.
const MAX_ARGUMENTS: usize = 256;

impl Invocation {
    /// What `arguments` ask the interpreter to run, with `standard_input` as what the command
    /// would read on standard input and `files` as the machine's own storage.
    ///
    /// The option grammar is CPython's own, in the forms a script written against an
    /// interpreter uses: the program on [`COMMAND_OPTION`], the module on [`MODULE_OPTION`],
    /// standard input for [`STANDARD_INPUT_OPERAND`] and a file of the machine's storage for a
    /// path — with the words after the program its own arguments, as `sys.argv` carries them.
    /// An option this reading does not perform is reported as the interpreter's own usage
    /// failure rather than performed as something else.
    fn read(
        arguments: &[String],
        standard_input: Option<&str>,
        files: &dyn ProgramFiles,
    ) -> Self {
        let mut words = arguments.iter().map(String::as_str);
        while let Some(word) = words.next() {
            if word == COMMAND_OPTION {
                let Some(source) = words.next() else {
                    return Self::usage(format!("Argument expected for the {COMMAND_OPTION} option"));
                };
                return Self::source(source, COMMAND_FILE, COMMAND_OPTION, words);
            }
            if word == MODULE_OPTION {
                let Some(module) = words.next() else {
                    return Self::usage(format!("Argument expected for the {MODULE_OPTION} option"));
                };
                return Self::module(module, words);
            }
            if word == STANDARD_INPUT_OPERAND {
                return Self::from_standard_input(standard_input, words);
            }
            if VALUED_OPTIONS.contains(&word) {
                if words.next().is_none() {
                    return Self::usage(format!("Argument expected for the {word} option"));
                }
                continue;
            }
            if IGNORED_OPTIONS.contains(&word) {
                continue;
            }
            if word == "-V" || word == "--version" {
                // The release is the interpreter's OWN, read off the interpreter running the
                // program rather than written out here.
                return Self::source(VERSION_PROGRAM, COMMAND_FILE, COMMAND_OPTION, words);
            }
            if word.starts_with('-') {
                return Self::Usage {
                    message: format!("Unknown option: {word}"),
                    status: USAGE_FAILURE,
                };
            }
            // A path operand: the program is the file the machine's storage holds there, which
            // is where a script that WROTE a program runs it from.
            let Some(source) = files.read(word) else {
                return Self::Usage {
                    message: format!(
                        "can't open file '{word}': [Errno 2] No such file or directory"
                    ),
                    status: USAGE_FAILURE,
                };
            };
            if source.len() > MAX_PROGRAM_BYTES {
                return Self::too_large();
            }
            return Self::source(&source, word, word, words);
        }
        // No program named at all: the interpreter reads one from standard input, as it does
        // when it is given neither a file nor a command.
        Self::from_standard_input(standard_input, words)
    }

    /// The invocation of a program read from STANDARD INPUT — a here-document, a pipeline's
    /// earlier stage, or the file an input redirection named.
    fn from_standard_input<'a>(
        standard_input: Option<&str>,
        rest: impl Iterator<Item = &'a str>,
    ) -> Self {
        match standard_input {
            Some(source) if source.len() > MAX_PROGRAM_BYTES => Self::too_large(),
            Some(source) => Self::source(
                source,
                STANDARD_INPUT_FILE,
                STANDARD_INPUT_OPERAND,
                rest,
            ),
            None => Self::Nothing,
        }
    }

    /// The invocation of the program `source`, whose frames name `file`, with `first` as the
    /// word the program reads as `sys.argv[0]` and `rest` as the arguments after it.
    fn source<'a>(
        source: &str,
        file: &str,
        first: &str,
        rest: impl Iterator<Item = &'a str>,
    ) -> Self {
        Self::Program {
            event: ::serde_json::json!({
                "source": source,
                "file": file,
                "argv": argv(first, rest),
            }),
        }
    }

    /// The invocation of the MODULE `module`, run as `__main__` the way `-m` runs one.
    fn module<'a>(module: &str, rest: impl Iterator<Item = &'a str>) -> Self {
        Self::Program {
            event: ::serde_json::json!({
                "module": module,
                "argv": argv(module, rest),
            }),
        }
    }

    /// The usage failure of an option written without the value it takes.
    fn usage(message: String) -> Self {
        Self::Usage {
            message,
            status: USAGE_FAILURE,
        }
    }

    /// The failure of a program larger than this interpreter reads at all.
    fn too_large() -> Self {
        Self::Usage {
            message: format!(
                "the program is larger than the {MAX_PROGRAM_BYTES} bytes one invocation may \
                 carry"
            ),
            status: USAGE_FAILURE,
        }
    }
}

/// What a program reads as `sys.argv`: the word naming the program itself, then the words
/// after it, bounded by [`MAX_ARGUMENTS`].
fn argv<'a>(first: &str, rest: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut argv = vec![first.to_string()];
    argv.extend(rest.take(MAX_ARGUMENTS).map(str::to_string));
    argv
}

/// The program `-V` runs: the release of the interpreter the program is running on, in the one
/// line CPython writes for that option.
const VERSION_PROGRAM: &str = "import sys\n\
     print('Python ' + '.'.join(str(part) for part in sys.version_info[:3]))\n";

/// Run `event`'s program in the contained interpreter and answer what it came to — or, where
/// this emulator will run no program for this command, report exactly that.
///
/// Three bounds are taken BEFORE a line of the program is read, and each of them is the host's:
///
/// * one of this process's run PLACES ([`RUN_PLACES`]), waited for only a bounded span and
///   charged to the request whether or not a program then runs, so a program never queues
///   behind another request's for as long as that one lasts and the interpreters standing at one
///   instant are bounded;
/// * the REQUEST's own allowance ([`ProgramAllowance`]), which every program of every script
///   that request delivers spends from, so what one request can make this process run is
///   bounded whatever its script and its node selection come to;
/// * the run's own budget and clock ([`run_budget`], [`HOST_CEILING`]).
///
/// None of those waits happens on a worker of the asynchronous runtime: the caller serving a
/// request performs the whole delivery on a blocking thread
/// ([`super::guest_execution::perform_delivery`]), which is what keeps that request cancellable
/// while a program runs.
///
/// The run happens on a THREAD OF ITS OWN, given the stack a run of caller-supplied code is
/// given ([`RUN_STACK_BYTES`]): the interpreter is recursive over the program's own syntax, so
/// a program inside the recursion limit the engine clamps is answered by that limit rather than
/// by the stack of whichever thread served the request. That thread holds the place and is
/// waited for no longer than [`HOST_WAIT`] — past which the run has been given up on, the
/// command is answered, and the thread keeps its place until it really ends — so the command
/// that invoked the interpreter is never left waiting on it. A run that unwound is contained ON
/// that thread, because this is performed with the guest's own store guard held (the one place
/// every program of a guest runs), so an unwind reaching the thread that served the request
/// would poison that guard for every later caller.
fn performed(
    name: &str,
    event: &::serde_json::Value,
    environment: &BTreeMap<String, String>,
    allowance: &Arc<ProgramAllowance>,
) -> ProgramStreams {
    // ONE of this process's places first, so a request that waits for one is charged the wait
    // whether or not it goes on to run a program.
    let began = ::std::time::Instant::now();
    let place = RUN_PLACES.taken();
    allowance.spent(began.elapsed());
    let Some(place) = place else {
        return not_run(
            name,
            &format!(
                "no interpreter of this emulator was free: it runs {MAX_CONCURRENT_PROGRAMS} \
                 programs at once"
            ),
        );
    };
    if !allowance.taken() {
        // The place goes straight back: nothing of this program runs.
        drop(place);
        return not_run(
            name,
            &format!(
                "this emulator has run the {MAX_PROGRAMS_PER_REQUEST} programs one request may \
                 run, or the host time they may take between them"
            ),
        );
    }
    let event = event.to_string();
    let environment = environment.clone();
    // The thread answers through this, so the command can give up on a run without giving up
    // the thread: a run this emulator stopped waiting for keeps its permit until it ends.
    let (sender, receiver) = ::std::sync::mpsc::sync_channel(1);
    let spawned = ::std::thread::Builder::new()
        .name("node-guest-python".to_string())
        .stack_size(RUN_STACK_BYTES)
        .spawn(move || {
            // Held for as long as the run really lasts, whoever is still waiting for it.
            let _place = place;
            let run = || {
                let modules = vec![PackageModule {
                    path: format!("{KERNEL_MODULE}.py"),
                    source: KERNEL_SOURCE.to_string(),
                }];
                python::run_against(
                    &python::Run {
                        modules: &modules,
                        handler: KERNEL_HANDLER,
                        // The environment the program runs with: the variables the session
                        // holds, which the engine installs where a program reads them
                        // (`os.environ`) before any of its code is read. Nothing of THIS
                        // process's own environment is in it — the session's variables are the
                        // machine's and the script's.
                        environment: &environment,
                        // The instruction set the environment the program runs in has, which
                        // the run reports as its own machine. The guest names none here — what
                        // `uname` answers in the guest is the machine's own program's answer —
                        // so it is the one the engine reports when none is named.
                        architecture:
                            ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                        event: Some(&event),
                        request_id: "",
                        invoked_function_arn: "",
                        client_context: None,
                        tenant_id: None,
                        budget: run_budget(),
                        // The program reaches no other service of this emulator: this run is
                        // given no seam to dispatch an AWS call on.
                        aws: None,
                        // It is no Lambda durable execution's run either, so it takes no step
                        // of one and checkpoints nothing onto one.
                        durable: None,
                    },
                    // The clock the run's own span is read against: one the RUN drives, a tick
                    // per traced event it delivers and the waits it asks for, stating beside it
                    // what the run may take of THIS host.
                    &Clock::driven(CLOCK_TICK).within(HOST_CEILING),
                )
            };
            let contained =
                ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(run)).ok();
            let _ = sender.send(contained);
        });
    if spawned.is_err() {
        return not_run(name, "the program could not be given a thread to run on");
    }
    match receiver.recv_timeout(HOST_WAIT) {
        Ok(Some(finished)) => {
            // What the run took of THIS host is charged to the request, so the programs of one
            // request cannot spend more of it between them than the allowance allows.
            allowance.spent(finished.elapsed);
            answered(name, &finished.completion)
        }
        // The thread ended without answering: there is no outcome of a program to report.
        Ok(None) => not_run(name, "the program ended without answering"),
        // The thread has still not answered a grace period past the host time one run may take
        // — which the engine has by then stopped the run on — so this emulator gave up on it,
        // and the command says exactly that.
        Err(_) => {
            allowance.spent(HOST_WAIT);
            not_run(
                name,
                &format!(
                    "the program was given up on: it had not answered within the {} seconds \
                     this emulator waits for one run",
                    HOST_WAIT.as_secs()
                ),
            )
        }
    }
}

/// What the command reports for a run the engine answered for.
fn answered(name: &str, completion: &Completion) -> ProgramStreams {
    match completion {
        Completion::Returned(returned) => read_answer(name, returned),
        // The engine's LATCH: a run whose budget was spent is a stopped run whatever the
        // program went on to return or catch, so the bound is what this reports.
        Completion::Stopped(stop) => ProgramStreams {
            status: RAISED,
            output: Vec::new(),
            errors: vec![format!("{name}: {}", stopped(*stop))],
        },
        // The kernel reports the program's own exception itself, so one that reaches here is
        // the kernel's or the interpreter's rather than the program's.
        Completion::Raised {
            error_type,
            error_message,
            ..
        } => ProgramStreams {
            status: RAISED,
            output: Vec::new(),
            errors: vec![format!("{name}: {error_type}: {error_message}")],
        },
        Completion::Internal(detail) => not_run(name, detail),
        Completion::Streamed { .. } => {
            not_run(name, "the program's answer was not the one the kernel reports")
        }
    }
}

/// How a run the engine STOPPED is reported, in the words of the bound it ran past.
fn stopped(stop: Stop) -> String {
    match stop {
        Stop::Deadline => format!(
            "the program was stopped after the {} seconds one program of this guest may run",
            RUN_CEILING.as_secs()
        ),
        Stop::HostCeiling => format!(
            "the program was stopped: it took more of this host's own time than the {} seconds \
             one program of this guest may take of it",
            HOST_CEILING.as_secs()
        ),
        Stop::Steps => "the program was stopped after spending its step budget".to_string(),
        Stop::Memory => {
            "the program was stopped after taking more memory than one program of this guest may"
                .to_string()
        }
        Stop::Response => {
            "the program was stopped: it wrote more than one program of this guest may report"
                .to_string()
        }
    }
}

/// What a run that did not happen reports: the reason on standard error and a non-zero status,
/// and no output for work nothing performed.
fn not_run(name: &str, reason: &str) -> ProgramStreams {
    ProgramStreams {
        status: RAISED,
        output: Vec::new(),
        errors: vec![format!("{name}: {reason}")],
    }
}

/// The two streams and the status the kernel ANSWERED with, as the command reports them.
///
/// Each stream is reported in the LINES the program wrote it in, which is what the session
/// writes to its own standard output and standard error; a stream the program left unterminated
/// is still the line it wrote.
fn read_answer(name: &str, answer: &str) -> ProgramStreams {
    let Ok(document) = ::serde_json::from_str::<::serde_json::Value>(answer) else {
        return not_run(name, "the program's answer could not be read");
    };
    let lines = |member: &str| -> Vec<String> {
        let written = document
            .get(member)
            .and_then(::serde_json::Value::as_str)
            .unwrap_or_default();
        let mut lines: Vec<String> = written.split('\n').map(str::to_string).collect();
        // A stream ending in a line break describes no line after the break.
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    };
    ProgramStreams {
        status: document
            .get("exitCode")
            .and_then(::serde_json::Value::as_i64)
            .and_then(|status| i32::try_from(status).ok())
            .unwrap_or(SUCCESS),
        output: lines("stdout"),
        errors: lines("stderr"),
    }
}

/// The KERNEL one run resolves: the program that executes what the invocation named as
/// `__main__` and answers with the two streams it wrote and the status it ended on.
///
/// Ordinary Python, the way every module this workspace hands the interpreter is. It is NOT the
/// containment — that is the engine's, in host code the program cannot reach — it is the
/// INVOCATION: the program's `sys.argv`; its two output streams kept APART, because the
/// invocation reports each as a member of its own; and the status it ended on, which is zero
/// for a program that ran to its end, the status a `SystemExit` named, and one for a program
/// that ended on an uncaught exception, as CPython exits for each. The program's own
/// ENVIRONMENT is not this kernel's either: the host installs it before this runs.
const KERNEL_SOURCE: &str = r#"
"""The program one `python3` invocation of a node's guest runs."""

import io
import sys
import traceback


def _ran(event, namespace):
    """Execute what the invocation named, as `__main__`."""
    module = event.get("module")
    if module:
        import runpy

        runpy.run_module(module, run_name="__main__", alter_sys=True)
        return
    source = event.get("source") or ""
    name = event.get("file") or "<stdin>"
    exec(compile(source, name, "exec"), namespace)


def _reported(out, err):
    """The traceback of the exception in flight, as the interpreter writes one.

    The frame this kernel ran the program from is dropped, so the traceback names the
    program's own frames and nothing of the kernel around them.
    """
    kind, value, frames = sys.exc_info()
    inner = frames.tb_next if frames is not None else None
    err.write("".join(traceback.format_exception(kind, value, inner or frames)))


def main(event, context):
    out = io.StringIO()
    err = io.StringIO()
    namespace = {"__name__": "__main__", "__file__": event.get("file") or ""}
    held_out, held_err, held_argv = sys.stdout, sys.stderr, sys.argv
    sys.stdout, sys.stderr = out, err
    sys.argv = list(event.get("argv") or [""])
    status = 0
    try:
        try:
            _ran(event, namespace)
        except SystemExit as ended:
            code = ended.code
            if code is None:
                status = 0
            elif isinstance(code, int):
                status = code
            else:
                # "If another type of object is passed, … it is printed to stderr and
                # results in an exit code of 1."
                err.write(str(code) + "\n")
                status = 1
        except BaseException:
            _reported(out, err)
            status = 1
    finally:
        sys.stdout, sys.stderr, sys.argv = held_out, held_err, held_argv
    return {"stdout": out.getvalue(), "stderr": err.getvalue(), "exitCode": status}
"#;
