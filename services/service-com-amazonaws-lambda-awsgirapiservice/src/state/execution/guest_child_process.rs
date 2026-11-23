// SPDX-License-Identifier: CC0-1.0

//! The CHILD a handler starts: one program of the execution environment, resolved against that
//! environment's own `PATH` and mounts and PERFORMED over its filesystem, in this process.
//!
//! A managed runtime's execution environment is an ordinary POSIX sandbox: it has a shell and
//! the usual utilities in its system directories, "Lambda extracts the layer contents into the
//! `/opt` directory" and puts `/opt/bin` on the environment's `PATH`, so a handler that runs a
//! program — a utility of the sandbox, or an executable its own layer ships — gets that
//! program's output and its exit status. This module is what answers such a call: the
//! child-process interface a handler imports ([`super::guest_process`]) hands the program, its
//! operands and the FILES of the environment over, and what comes back is the status and the
//! two streams the program really produced.
//!
//! # What performs the program
//!
//! The ONE bounded, in-process POSIX shell session this workspace performs caller-supplied
//! shell programs with ([`::service_common::posix_shell_session`], which ECS, Systems Manager,
//! CodeBuild and EMR drive the same way). Nothing here starts a host process and nothing opens
//! a host path: the program is read by that module's grammar and performed over a tree held in
//! this process's memory, so every command it names is either one that session provides or one
//! that did not run.
//!
//! # How a program is RESOLVED
//!
//! The shell's own command search, over the environment's own filesystem:
//!
//! * a command NAME carrying a slash is the file at that path, resolved against the directory
//!   the caller started the child in;
//! * a bare name is searched for in the directories the environment's `PATH` names, in order —
//!   which is what makes an executable a layer ships under `/opt/bin` reachable by its bare
//!   name;
//! * a directory of the sandbox's own utilities ([`SYSTEM_DIRECTORIES`]) provides the programs
//!   that session performs itself ([`is_system_program`]), which is what the sandbox has
//!   installed in `/bin` and `/usr/bin`;
//! * the environment's SHELL is that session itself, so a child started as `sh -c …` (which is
//!   what a `shell=True` call and `os.popen` start) performs its command line here;
//! * a name nothing resolves is where the search ends with nothing, reported as the
//!   `No such file or directory` the call raises ([`Failure::NOT_FOUND`]) rather than as a
//!   program that ran — unless the sandbox really HAS that program and this emulator has no
//!   engine for it ([`PROGRAMS_WITHOUT_AN_ENGINE`]), which is the capability refusal instead.
//!
//! That search is the one the SESSION performs too: a name inside a command line, inside an
//! `sh -c` program and inside a script a resolved program itself runs is resolved against the
//! same tree and the same `PATH`, by the session's own command search
//! ([`::service_common::posix_shell_session`]'s `performed_tree_program`) — so a layer's
//! program runs identically whichever form named it. What this module adds on top is what
//! Python's own `exec` says about a program that never started: WHICH error the call raises
//! for a name nothing resolved, and for one no engine here can run.
//!
//! # What is NOT a program this runs
//!
//! A file whose own interpreter line names something other than the environment's shell — a
//! native executable a layer ships, a program written for another interpreter — needs a
//! capability this emulator does not have, and it is reported as that rather than as work that
//! was done ([`Failure::UNSUPPORTED`]): no exit status, no output and no process id is invented
//! for a program nothing performed. A text file with no interpreter line is run by the shell,
//! which is what a shell does with a file it could not execute itself.
//!
//! # The filesystem the child runs over
//!
//! The environment's own: the read-only mounts the handler reads ([`super::guest_mount`]) and
//! the writable ephemeral storage beside them ([`super::guest_ephemeral`]), both held in the
//! guest and handed over per call. The mount roots are sealed as the read-only filesystems they
//! are ([`::service_common::posix_shell_volume::ShellVolume::seal_directory`]), so a child's
//! write under one fails exactly as the handler's own does, and what the child wrote anywhere
//! else is answered back for the guest to apply to its own storage — so a file a child left in
//! `/tmp` is a file the handler reads afterwards.
//!
//! What that tree holds is bounded, and bounded in the guest ([`super::guest_process`]): the
//! files it hands over are the ones whose bytes are TEXT, up to its own count and byte
//! ceilings. A child therefore reads the text files of the environment and not an entry whose
//! bytes are not text — the shell performs work over text — while the handler itself reads
//! every entry of its mounts, text or not.
//!
//! # Bounds
//!
//! Every bound is this host's, held where the guest cannot reach it: how many commands one
//! child performs, how long its program may be, how much of each stream is kept
//! ([`child_session_config`]) and how much of a tree it may hold ([`child_volume_limits`]). The
//! program is performed on a thread of its own, sized for the nesting those bounds allow, and
//! an unwind on it answers "nothing ran" rather than travelling into the interpreter that asked.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};

use ::service_common::posix_shell_session::{
    command_base_name, script_interpreter, stream_text, Bounds, OutputLimits, SessionConfig,
    ShellSession, BUILT_IN_COMMANDS, NESTED_SHELL_NAMES, VOLUME_COMMANDS,
};
use ::service_common::posix_shell_text_filters::TEXT_FILTER_COMMANDS;
use ::service_common::posix_shell_volume::{resolved_path, ShellVolume, VolumeLimits};

/// The directories a managed runtime's sandbox has its OWN utilities installed in, which is
/// where the programs the shell session performs itself are found. "PATH –
/// /usr/local/bin:/usr/bin/:/bin:/opt/bin" names the first three of them.
const SYSTEM_DIRECTORIES: &[&str] = &["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Whether those directories hold a program called `name`: the utilities the shell session
/// performs ITSELF are what the sandbox has installed in them, so the catalogs are read from
/// that session rather than restated here — a utility added to one of them is a program of this
/// sandbox without a second list having to be edited.
///
/// The names a shell performs as BUILT-INS and nothing else ([`SHELL_ONLY_COMMANDS`]) are the
/// exception: a child process is a PROGRAM, and those are not programs a sandbox has.
fn is_system_program(name: &str) -> bool {
    if SHELL_ONLY_COMMANDS.contains(&name) {
        return false;
    }
    BUILT_IN_COMMANDS.contains(&name)
        || VOLUME_COMMANDS.contains(&name)
        || TEXT_FILTER_COMMANDS.contains(&name)
}

/// The commands of those catalogs a shell performs as BUILT-INS, which no sandbox has a
/// program for: ending the shell, changing its own variables or options, leaving a loop, and
/// reading a file's commands into the shell itself.
const SHELL_ONLY_COMMANDS: &[&str] =
    &["exit", "export", "unset", "set", "break", "continue", ":", ".", "source"];

/// The programs a managed runtime's sandbox really has installed and this emulator has no
/// engine for: a handler naming one of them has named something the environment HAS, so it is
/// answered as the capability this emulator is missing ([`Failure::UNSUPPORTED`]) rather than
/// as a file that is not there.
///
/// The interpreters and the archivers, compressors, stream editors and transfer programs a
/// Python runtime's image carries beside them. A name on neither this list nor the catalogs
/// above is one nothing in the environment provides, which is the other answer.
const PROGRAMS_WITHOUT_AN_ENGINE: &[&str] = &[
    "python", "python2", "python3", "pip", "pip3", "perl", "ruby", "node", "npm", "java", "sed",
    "awk", "gawk", "tar", "gzip", "gunzip", "zip", "unzip", "bzip2", "xz", "curl", "wget",
    "openssl", "make", "git", "find", "xargs", "dd", "ps", "kill", "env", "nohup", "timeout",
];

/// The name the environment's own SHELL is invoked under, which is what `$0` is worth in a
/// command line it was handed.
const SHELL_NAME: &str = "sh";

/// The directory a child is started in when its caller named none, which is the directory a
/// Lambda function's own code is at.
const TASK_ROOT: &str = "/var/task";

/// The `PATH` a child is resolved against when the environment publishes none at all.
const DEFAULT_SEARCH_PATH: &str = "/usr/local/bin:/usr/bin/:/bin:/opt/bin";

/// The variable the search path is published in.
const SEARCH_PATH_VARIABLE: &str = "PATH";

/// How many commands ONE child performs before the session is given up on, how long its
/// program may be and how many nested groups it may carry.
///
/// Every one of them is this host's number — the program is the function's own text — and they
/// are stated for a SCRIPT rather than for a line, because a program a layer ships is a script
/// read in one piece. They are the numbers the other callers of that session give a script
/// (`…ec2…::state::guest_commands`).
const MAX_CHILD_COMMANDS: usize = 1_000;

/// See [`MAX_CHILD_COMMANDS`].
const MAX_CHILD_PROGRAM_BYTES: usize = 64 * 1024;

/// See [`MAX_CHILD_COMMANDS`].
const MAX_CHILD_PROGRAM_NESTING: usize = 256;

/// How many lines of each stream one child's output keeps, and how many bytes one of those
/// lines carries. A program that wrote more keeps the first of it, which is what a caller
/// reading the head of a command's output sees.
const MAX_CHILD_OUTPUT_LINES: usize = 1_024;

/// See [`MAX_CHILD_OUTPUT_LINES`].
const MAX_CHILD_OUTPUT_LINE_BYTES: usize = 8 * 1024;

/// How many files the tree one child runs over holds, and how many bytes they come to: the
/// mounts and the ephemeral storage the guest hands over, and whatever the program writes.
const MAX_CHILD_FILES: usize = 1_024;

/// See [`MAX_CHILD_FILES`].
const MAX_CHILD_TREE_BYTES: usize = 8 * 1024 * 1024;

/// The stack the thread one child's program is performed on is given: far above what a program
/// nesting [`MAX_CHILD_PROGRAM_NESTING`] groups costs the grammar's recursive descent and the
/// session's own walk over what it parsed, so a program inside that bound is answered by the
/// bound rather than by the stack it ran on.
const MAX_CHILD_STACK_BYTES: usize = 16 * 1024 * 1024;

/// The process id the NEXT child that really runs in this process reports.
///
/// A child that ran has an identity of its own, and this is it: ids are handed out as children
/// run, so two children never report one id and a call that performed nothing is handed none at
/// all. It begins above the id the environment's own process reports
/// ([`super::host_posix`]'s `getpid`).
static NEXT_CHILD_ID: AtomicU32 = AtomicU32::new(2);

/// What one call of the seam asked for: the program, where it runs and what it may read.
#[derive(Debug, Default)]
struct Request {
    /// The program and its operands, as the caller passed them — empty for a call that names a
    /// command LINE instead.
    argv: Vec<String>,
    /// The command LINE of a shell invocation, which is what a `shell=True` call carries.
    program: Option<String>,
    /// The program to run in place of the first word, when the caller named one.
    executable: Option<String>,
    /// The directory the child is started in.
    cwd: String,
    /// The environment the child is given, which is what its `PATH` is read from and what its
    /// own parameter expansions resolve against.
    environment: BTreeMap<String, String>,
    /// The bytes the caller handed the child on standard input, when it handed it any.
    input: Option<String>,
    /// The FILES of the execution environment, by absolute path: what the child may read, and
    /// what the tree it writes starts out holding.
    files: BTreeMap<String, String>,
    /// The DIRECTORIES of that environment, by absolute path: the ones its mounts and its
    /// ephemeral storage hold, including the ones holding no file at all — a child writes into
    /// a directory the environment has and into no other.
    directories: Vec<String>,
    /// The roots of the environment's READ-ONLY mounts, which nothing the child does may write.
    read_only_roots: Vec<String>,
}

/// Why no child ran at all, as the call reports it.
#[derive(Debug)]
struct Failure {
    /// The number the call raises with, which is what a program branches on.
    errno: i32,
    /// What the failure says.
    message: String,
    /// The path that was being resolved, for the error the caller raises.
    path: String,
}

impl Failure {
    /// `ENOENT`: the command search ended with nothing, which is what a call naming a program
    /// the environment does not have raises.
    const NOT_FOUND: i32 = 2;

    /// `ENOSYS`: a program this emulator has no engine for — nothing ran, and nothing about a
    /// run is reported for it.
    const UNSUPPORTED: i32 = 38;

    /// The command search found nothing for `named`.
    fn not_found(named: &str) -> Self {
        Self {
            errno: Self::NOT_FOUND,
            message: "No such file or directory".to_string(),
            path: named.to_string(),
        }
    }

    /// `named` is a program this emulator does not run: it is not text the execution
    /// environment's own shell performs, and there is no host process here to execute it in.
    fn unsupported(named: &str, interpreter: &str) -> Self {
        Self {
            errno: Self::UNSUPPORTED,
            message: format!(
                "a Lambda function in this emulator runs only a program its execution \
                 environment's own shell performs, and {named} is run by {interpreter}"
            ),
            path: named.to_string(),
        }
    }
}

/// What the command search resolved a call to.
#[derive(Debug)]
enum Resolved {
    /// A command LINE, performed by the environment's own shell — what a `shell=True` call and
    /// a child started as `sh -c …` are.
    Line(String),
    /// A SCRIPT the environment holds, with the positional parameters it was invoked with: the
    /// name it was invoked under and the operands after it.
    Script {
        /// The text the file holds.
        text: String,
        /// `$0` and the operands after it.
        arguments: Vec<String>,
    },
    /// Nothing ran: the search found no program, or found one this emulator does not run.
    Refused(Failure),
}

/// What one child that RAN came to.
#[derive(Debug)]
struct Child {
    /// The id this child reported.
    pid: u32,
    /// The status it exited with.
    status: i32,
    /// What it wrote to standard output.
    stdout: String,
    /// What it wrote to standard error.
    stderr: String,
    /// The files of the tree it ran over that are not under a read-only mount: the writable
    /// storage as the child left it, for the guest to apply to its own.
    files: BTreeMap<String, String>,
}

/// Perform one child the guest asked for, as a JSON document in and a JSON document out —
/// the form every host seam of this runtime takes ([`super::python`]'s host module).
///
/// The answer is either a child that RAN (`started`, with its id, its status, its two streams
/// and the writable files it left behind) or the reason none did (`failure`, with the number and
/// the message the call raises): no status, no output and no id is reported for work nothing
/// performed.
#[must_use]
pub fn answered(document: &str) -> String {
    let request = read_request(document);
    let volume = child_volume(&request);
    match resolve(&request, &volume) {
        Resolved::Refused(failure) => refusal_document(&failure),
        Resolved::Line(line) => run_in(&request, &volume, &line, vec![SHELL_NAME.to_string()]),
        Resolved::Script { text, arguments } => run_in(&request, &volume, &text, arguments),
    }
}

/// Perform `program` in a session over `volume`, with `arguments` as the positional parameters
/// it was invoked with, and answer the document the guest reads.
fn run_in(
    request: &Request,
    volume: &ShellVolume,
    program: &str,
    arguments: Vec<String>,
) -> String {
    let session = ShellSession::in_volume(
        request.environment.clone(),
        child_session_config(),
        volume.clone(),
    )
    // The two streams are reported as members of their own, the way a child's are.
    .reporting_both_streams()
    .in_directory(&request.cwd)
    .with_arguments(arguments);
    let session = match &request.input {
        Some(input) => session.with_standard_input(input.clone()),
        None => session,
    };
    match performed(session, program, &request.read_only_roots) {
        Some(child) => ::serde_json::json!({
            "started": true,
            "pid": child.pid,
            "status": child.status,
            "stdout": child.stdout,
            "stderr": child.stderr,
            "files": child.files,
        })
        .to_string(),
        None => refusal_document(&Failure {
            errno: Failure::UNSUPPORTED,
            message: "a Lambda function in this emulator could not perform the program at all"
                .to_string(),
            path: String::new(),
        }),
    }
}

/// The document a call that started no child answers with.
fn refusal_document(failure: &Failure) -> String {
    ::serde_json::json!({
        "started": false,
        "failure": {
            "errno": failure.errno,
            "message": failure.message,
            "path": failure.path,
        },
    })
    .to_string()
}

/// Perform `program` in `session` and answer what it came to — `None` for a program this
/// process could not perform at all.
///
/// The run happens on a THREAD of its own with a stack sized for the nesting bound the session
/// was constructed with ([`MAX_CHILD_STACK_BYTES`]), and inside
/// [`::std::panic::catch_unwind`] on that thread: a program inside the bounds is answered by
/// the bounds, and an unwind becomes "nothing ran" instead of travelling into the interpreter
/// that asked for the child.
fn performed(
    mut session: ShellSession,
    program: &str,
    read_only_roots: &[String],
) -> Option<Child> {
    let roots: Vec<String> = read_only_roots.to_vec();
    let run = move || {
        let mut status = session.run(program);
        if let Some(exited) = session.exit_status() {
            // The program ended the session itself: that status is the child's.
            status = exited;
        }
        // The two streams of a child are the lines the session kept, as the bytes they are
        // ([`::service_common::posix_shell_session::stream_text`]).
        let stdout = stream_text(session.output());
        let stderr = stream_text(session.errors());
        let files = match session.into_volume() {
            Some(volume) => volume
                .stored_files()
                .filter(|(path, _)| !under_any(path, &roots))
                .map(|(path, contents)| (path.to_string(), contents.to_string()))
                .collect(),
            None => BTreeMap::new(),
        };
        Child {
            pid: NEXT_CHILD_ID.fetch_add(1, Ordering::Relaxed),
            status,
            stdout,
            stderr,
            files,
        }
    };
    let contained = move || ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(run)).ok();
    ::std::thread::scope(|scope| {
        ::std::thread::Builder::new()
            .stack_size(MAX_CHILD_STACK_BYTES)
            .spawn_scoped(scope, contained)
            .ok()
            .and_then(|thread| thread.join().ok())
            .flatten()
    })
}

/// Whether `path` is at or under one of `roots`.
fn under_any(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    })
}

/// What one child may do, which is what bounds the work one call of this seam costs.
fn child_session_config() -> SessionConfig {
    SessionConfig {
        bounds: Bounds {
            commands: MAX_CHILD_COMMANDS,
            command_bytes: MAX_CHILD_PROGRAM_BYTES,
            nesting: MAX_CHILD_PROGRAM_NESTING,
            ..Bounds::default()
        },
        output: Some(OutputLimits {
            lines: MAX_CHILD_OUTPUT_LINES,
            line_bytes: MAX_CHILD_OUTPUT_LINE_BYTES,
        }),
        ..SessionConfig::default()
    }
}

/// How much the tree one child runs over may hold.
fn child_volume_limits() -> VolumeLimits {
    VolumeLimits {
        files: MAX_CHILD_FILES,
        bytes: MAX_CHILD_TREE_BYTES,
        directories: MAX_CHILD_FILES,
        ..VolumeLimits::default()
    }
}

/// The tree the child runs over: the directories and files the guest handed over, with the
/// read-only mounts sealed as the read-only filesystems they are.
///
/// The environment's own entries are seeded FIRST and the roots sealed after, because seeding
/// is the environment's own and not a program's write: what the mounts carry is readable, and
/// every write, creation and removal a program attempts under one of those roots then fails the
/// way it fails on the real runtime.
fn child_volume(request: &Request) -> ShellVolume {
    let mut volume = ShellVolume::new(child_volume_limits());
    for directory in &request.directories {
        volume.make_directories(directory);
    }
    for (path, contents) in &request.files {
        if under_any(path, &request.read_only_roots) {
            volume.seal_file(path, contents);
        } else {
            volume.put_file(path, contents);
        }
    }
    for root in &request.read_only_roots {
        volume.seal_directory(root);
    }
    volume
}

/// The command search: what the call names, resolved against the environment's own filesystem
/// and `PATH`. See the module docs.
fn resolve(request: &Request, volume: &ShellVolume) -> Resolved {
    if let Some(line) = &request.program {
        return Resolved::Line(line.clone());
    }
    let Some((first, operands)) = request.argv.split_first() else {
        return Resolved::Refused(Failure::not_found(""));
    };
    let named = request
        .executable
        .clone()
        .filter(|named| !named.is_empty())
        .unwrap_or_else(|| first.clone());
    let base = command_base_name(&named);
    // The environment's SHELL is the session itself, so a child started as `sh …` performs its
    // own operands here — `-c PROGRAM`, or the script the environment holds at that path.
    if NESTED_SHELL_NAMES.contains(&base) && !volume.is_file(&at(&request.cwd, &named)) {
        let mut words = vec![SHELL_NAME.to_string()];
        words.extend(operands.iter().cloned());
        return Resolved::Line(command_line(&words));
    }
    // "A command name containing a slash is the file at that path": there is no search for one.
    if named.contains('/') {
        let path = at(&request.cwd, &named);
        if let Some(text) = file_text(volume, &path) {
            return script(&named, text, &request.argv);
        }
        if let Some(line) = system_program_line(&path, &named, operands) {
            return Resolved::Line(line);
        }
        return Resolved::Refused(absent_program(&named));
    }
    for directory in search_path(&request.environment) {
        let path = format!("{}/{base}", directory.trim_end_matches('/'));
        if let Some(text) = file_text(volume, &path) {
            return script(&named, text, &request.argv);
        }
        if let Some(line) = system_program_line(&path, &named, operands) {
            return Resolved::Line(line);
        }
    }
    Resolved::Refused(absent_program(&named))
}

/// What a call naming a program nothing resolved is answered with: the capability this emulator
/// is missing for a program the sandbox really HAS and no engine here performs
/// ([`PROGRAMS_WITHOUT_AN_ENGINE`]), and the file that is not there for every other name — so
/// the answer is never a statement about the environment's contents that is untrue.
fn absent_program(named: &str) -> Failure {
    let base = command_base_name(named);
    if PROGRAMS_WITHOUT_AN_ENGINE.contains(&base) {
        return Failure::unsupported(named, base);
    }
    Failure::not_found(named)
}

/// The command line a program of the sandbox's OWN directories is performed as, and `None` for
/// a path that is not one of those directories or a name those directories hold no program
/// under.
fn system_program_line(path: &str, named: &str, operands: &[String]) -> Option<String> {
    let directory = path.rsplit_once('/').map(|(directory, _)| directory)?;
    if !SYSTEM_DIRECTORIES.contains(&directory) {
        return None;
    }
    let base = command_base_name(named);
    if !is_system_program(base) {
        return None;
    }
    let mut words = vec![base.to_string()];
    words.extend(operands.iter().cloned());
    Some(command_line(&words))
}

/// The SCRIPT at `named`, as the program to perform — or the refusal for a file this emulator
/// has no engine for.
///
/// A file whose first line names an interpreter is performed only when that interpreter is the
/// environment's own shell; a file with no interpreter line at all is performed by that shell,
/// which is what a shell does with a file it could not execute itself. The interpreter line is
/// read the way the session itself reads one
/// ([`::service_common::posix_shell_session::script_interpreter`]), so a program resolved here
/// and one resolved inside a line of that session are judged by one reading.
fn script(named: &str, text: String, argv: &[String]) -> Resolved {
    if let Some(interpreter) = script_interpreter(&text) {
        if !NESTED_SHELL_NAMES.contains(&command_base_name(&interpreter)) {
            return Resolved::Refused(Failure::unsupported(named, &interpreter));
        }
    }
    Resolved::Script {
        text,
        arguments: argv.to_vec(),
    }
}

/// `words` as ONE command line, each word quoted the way a shell quotes one, so what the
/// caller passed as separate operands stays separate words however they are spelled.
fn command_line(words: &[String]) -> String {
    words
        .iter()
        .map(|word| quoted(word))
        .collect::<Vec<String>>()
        .join(" ")
}

/// `word` as one word of a command line: itself when it carries nothing a shell reads, and
/// single-quoted otherwise, with its own single quotes spelled the way a shell spells one
/// inside them.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty()
        && word.chars().all(|character| {
            character.is_ascii_alphanumeric() || "_@%+=:,./-".contains(character)
        });
    if plain {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The directories the environment's `PATH` names, in the order it names them.
fn search_path(environment: &BTreeMap<String, String>) -> Vec<String> {
    let published = environment
        .get(SEARCH_PATH_VARIABLE)
        .filter(|path| !path.is_empty())
        .cloned()
        .unwrap_or_else(|| DEFAULT_SEARCH_PATH.to_string());
    published
        .split(':')
        .filter(|directory| !directory.is_empty())
        .map(str::to_string)
        .collect()
}

/// `named` resolved against the directory the child was started in, the way a process resolves
/// a path it was given.
fn at(cwd: &str, named: &str) -> String {
    if named.starts_with('/') {
        return resolved_path(named).unwrap_or_else(|| named.to_string());
    }
    let directory = if cwd.is_empty() { TASK_ROOT } else { cwd };
    let joined = format!("{}/{named}", directory.trim_end_matches('/'));
    resolved_path(&joined).unwrap_or(joined)
}

/// What the tree holds at `path`, when it holds a FILE there.
fn file_text(volume: &ShellVolume, path: &str) -> Option<String> {
    if !volume.is_file(path) {
        return None;
    }
    volume.read(path)
}

/// The call the guest made, read out of its document. A member that is not there is one the
/// caller did not name, and a document that is not readable at all asks for nothing: the
/// resolution then finds no program, which is what the call reports.
fn read_request(document: &str) -> Request {
    let Ok(parsed) = ::serde_json::from_str::<::serde_json::Value>(document) else {
        return Request::default();
    };
    let strings = |name: &str| -> Vec<String> {
        parsed
            .get(name)
            .and_then(::serde_json::Value::as_array)
            .map(|held| {
                held.iter()
                    .filter_map(::serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let text = |name: &str| -> Option<String> {
        parsed
            .get(name)
            .and_then(::serde_json::Value::as_str)
            .map(str::to_string)
    };
    let mapping = |name: &str| -> BTreeMap<String, String> {
        parsed
            .get(name)
            .and_then(::serde_json::Value::as_object)
            .map(|held| {
                held.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Request {
        argv: strings("argv"),
        program: text("program"),
        executable: text("executable"),
        cwd: text("cwd").unwrap_or_else(|| TASK_ROOT.to_string()),
        environment: mapping("environment"),
        input: text("input"),
        files: mapping("files"),
        directories: strings("directories"),
        read_only_roots: strings("readOnlyRoots"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One call of the seam, as the guest makes it.
    fn asked(document: ::serde_json::Value) -> ::serde_json::Value {
        let answered = answered(&document.to_string());
        ::serde_json::from_str(&answered).expect("the seam answers a JSON document")
    }

    /// The document a child of a layer's own program is started with.
    fn with_layer_program(argv: ::serde_json::Value) -> ::serde_json::Value {
        ::serde_json::json!({
            "argv": argv,
            "cwd": "/var/task",
            "environment": { "PATH": "/usr/local/bin:/usr/bin/:/bin:/opt/bin" },
            "files": {
                "/opt/bin/greet": "#!/bin/sh\necho \"hello $1\"\n",
                "/var/task/index.py": "def handler(event, context):\n    return {}\n",
            },
            "readOnlyRoots": ["/opt", "/var/task"],
            "directories": ["/opt", "/opt/bin", "/var/task", "/tmp"],
        })
    }

    #[test]
    fn a_layer_program_runs_by_its_bare_name_and_by_its_path() {
        for argv in [
            ::serde_json::json!(["greet", "world"]),
            ::serde_json::json!(["/opt/bin/greet", "world"]),
        ] {
            let answered = asked(with_layer_program(argv.clone()));
            assert_eq!(
                answered["started"], true,
                "case {argv}: expected the layer's program to run, got {answered}"
            );
            assert_eq!(
                answered["status"], 0,
                "case {argv}: expected the program's own success, got {answered}"
            );
            assert_eq!(
                answered["stdout"], "hello world\n",
                "case {argv}: expected the program's own output, got {answered}"
            );
        }
    }

    #[test]
    fn a_layer_program_runs_inside_a_command_line_and_inside_another_program() {
        // Each case: the command LINE a child is started with, and the output it must produce.
        // The program is named by its bare name on `PATH`, by its own path, from inside a
        // pipeline, and from inside a script of the layer that calls its sibling — one command
        // search, whichever form reaches it.
        for (line, expected) in [
            ("greet world", "hello world\n"),
            ("/opt/bin/greet world", "hello world\n"),
            ("greet world | cat", "hello world\n"),
            ("sh -c 'greet world'", "hello world\n"),
            ("caller", "hello from the sibling\n"),
        ] {
            let mut document = with_layer_program(::serde_json::json!([]));
            document["program"] = ::serde_json::json!(line);
            document["files"]["/opt/bin/caller"] =
                ::serde_json::json!("#!/bin/sh\ngreet \"from the sibling\"\n");
            let answered = asked(document);
            assert_eq!(
                answered["started"], true,
                "case {line:?}: expected the line to run, got {answered}"
            );
            assert_eq!(
                answered["status"], 0,
                "case {line:?}: expected the program's own success, got {answered}"
            );
            assert_eq!(
                answered["stdout"], expected,
                "case {line:?}: expected the program's own output, got {answered}"
            );
        }
    }

    #[test]
    fn a_name_the_environment_has_no_program_for_runs_nothing() {
        let answered = asked(with_layer_program(::serde_json::json!(["absent-tool"])));
        assert_eq!(
            answered["started"], false,
            "expected no child at all, got {answered}"
        );
        assert_eq!(
            answered["failure"]["errno"], Failure::NOT_FOUND,
            "expected the command search to end with nothing, got {answered}"
        );
    }

    #[test]
    fn a_program_the_sandbox_has_and_no_engine_performs_is_reported_as_that() {
        // A name the real sandbox really ships ([`PROGRAMS_WITHOUT_AN_ENGINE`]) is not a file
        // that is missing: it is the capability this emulator has none of.
        for argv in [
            ::serde_json::json!(["python3", "-c", "print(1)"]),
            ::serde_json::json!(["/usr/bin/sed", "-e", "s/a/b/"]),
        ] {
            let answered = asked(with_layer_program(argv.clone()));
            assert_eq!(
                answered["started"], false,
                "case {argv}: expected no child at all, got {answered}"
            );
            assert_eq!(
                answered["failure"]["errno"], Failure::UNSUPPORTED,
                "case {argv}: expected the capability refusal rather than an absent file, got \
                 {answered}"
            );
        }
    }

    #[test]
    fn a_program_of_another_interpreter_is_not_run() {
        let mut document = with_layer_program(::serde_json::json!(["/opt/bin/native"]));
        document["files"]["/opt/bin/native"] =
            ::serde_json::json!("#!/usr/bin/env python3\nprint('hi')\n");
        let answered = asked(document);
        assert_eq!(
            answered["started"], false,
            "expected no child at all, got {answered}"
        );
        assert_eq!(
            answered["failure"]["errno"], Failure::UNSUPPORTED,
            "expected the program to be reported as one this emulator does not run, got \
             {answered}"
        );
    }

    #[test]
    fn a_write_under_a_read_only_mount_is_refused_and_one_to_the_storage_is_answered_back() {
        let mut document = with_layer_program(::serde_json::json!([]));
        document["program"] = ::serde_json::json!(
            "echo kept > /tmp/kept; echo refused > /opt/bin/greet; cat /opt/bin/greet"
        );
        let answered = asked(document);
        assert_eq!(
            answered["started"], true,
            "expected the shell's own line to run, got {answered}"
        );
        assert_eq!(
            answered["files"]["/tmp/kept"], "kept\n",
            "expected the write to the ephemeral storage to be answered back, got {answered}"
        );
        assert!(
            answered["files"].get("/opt/bin/greet").is_none(),
            "expected nothing of a read-only mount to be answered back, got {answered}"
        );
        assert!(
            answered["stdout"]
                .as_str()
                .is_some_and(|written| written.contains("hello")),
            "expected the layer's own file to be unchanged, got {answered}"
        );
    }

    #[test]
    fn a_utility_of_the_sandbox_runs_and_reads_what_it_is_piped() {
        let mut document = with_layer_program(::serde_json::json!(["wc", "-l"]));
        document["input"] = ::serde_json::json!("one\ntwo\n");
        let answered = asked(document);
        assert_eq!(
            answered["started"], true,
            "expected the sandbox's own utility to run, got {answered}"
        );
        assert_eq!(
            answered["stdout"], "2\n",
            "expected the utility to count what it was given, got {answered}"
        );
    }
}
