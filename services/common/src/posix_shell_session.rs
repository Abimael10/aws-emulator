// SPDX-License-Identifier: CC0-1.0

//! The ONE bounded, session-local POSIX/bash SHELL SESSION every emulated service that has
//! to perform caller-supplied shell text drives.
//!
//! # What this module is
//!
//! Several emulated APIs carry a shell PROGRAM as a caller-supplied member: a build's
//! command lines, a container's `command`, a notebook instance's lifecycle hook. What the
//! client observes is what running that program produced — a status, the lines it wrote, the
//! variables it left behind — so a service that only STORES the text has implemented nothing.
//!
//! The grammar is not read here: a line is tokenized and parsed by `brush-parser`, the
//! POSIX/bash tokenizer and grammar, exactly as this workspace reads caller-supplied SQL
//! with `sqlparser` and caller-supplied YAML with `yaml-rust2`. That parser builds a syntax
//! tree and runs nothing, so the and-or lists, pipelines, assignments and quoting of a
//! program are the shell's own rather than something scanned out of the text. Word expansion
//! is read from the same parser's word pieces.
//!
//! # Why the grammar and not an engine
//!
//! The one maintained shell ENGINE this grammar has — `brush-core`, the executing half of
//! the same project — is deliberately not a dependency, and what follows is not a
//! reimplementation of it. An engine's whole purpose is to run what the shell language can
//! express: it resolves a command name against the host's `PATH` and starts a host PROCESS
//! for it, opens the host files a redirection names, and expands a command substitution by
//! starting that command as one. A host process and a host file are exactly the primitives
//! this module has none of, and the crate exposes no seam to replace them with an in-memory
//! volume or an in-process dispatcher — its command resolution and its redirections go
//! straight to the host through the standard library — so there is no configuration of it
//! that could be confined here: taking it would hand a caller's program the developer's
//! machine.
//!
//! What this module does instead is narrower than a shell by construction, and it is not an
//! implementation of the shell LANGUAGE: there is no evaluator for the language's values or
//! its expansions beyond the forms named here, and none is approximated. It PERFORMS a fixed
//! catalog of session-local utilities ([`BUILT_IN_COMMANDS`]) over the real grammar's tree,
//! beside the programs the caller's own environment declares it provides
//! ([`EnvironmentPrograms`]) — and, for a session that works over a tree, the grammar's
//! control flow around those same commands — and every other command, and every command FORM
//! and expansion it does not perform, fails closed with the status a shell answers for a
//! command it cannot run ([`Statuses::could_not_run`]). There is therefore no language subset
//! to keep in step with a specification: a command either names one of those, or it did not
//! run.
//!
//! # What a session provides
//!
//! It runs IN THIS PROCESS. It has no process to start, no network to reach, no HOST
//! filesystem and no host environment: the utilities it provides are the ones whose whole
//! effect is inside the session itself — its output, its status, its variables and the files
//! it holds itself. The variables it starts with are the ones its caller hands it, because
//! those are the environment the program is given.
//!
//! # The working tree
//!
//! A program's commands write FILES — a build writes the file its artifacts name, a hook
//! writes the configuration it was given — so a session holds a tree of its own: an output
//! redirection (`> path`, `>> path`) collects what the command wrote and keeps it under that
//! path in this session, and `cat` reads a path back out of it. The tree is session-local
//! memory, keyed by the path the program named and bounded like everything else
//! ([`Bounds::files`], [`Bounds::file_bytes`], [`Bounds::tree_bytes`]); no host file is
//! opened, created or read.
//!
//! A caller whose environment HAS storage of its own — a build container's source directory,
//! which the service seeds and then takes the produced files back out of — declares that
//! ([`ShellSession::with_file_utilities`]), and the file utilities ([`VOLUME_COMMANDS`]) are
//! then commands the session performs over that same tree. It is still the one tree above:
//! a path is a key, nothing holds a directory entry, and nothing reaches a host path.
//!
//! # The forms a session that works over a tree performs
//!
//! A program written against storage is written as a SCRIPT, so a session that was given a
//! tree — a machine's ([`ShellSession::in_volume`]) or its own working one — also performs
//! the grammar's own control flow over the commands inside it: `if`/`elif`/`else`, `while`,
//! `until` and `for`, and a brace group. Each of those runs the same catalog this module
//! performs and needs no primitive beyond it, each command inside spends the same budget
//! ([`Bounds::commands`]), and that budget is what ends a loop with no end of its own. A
//! subshell, a `case` clause, a function definition and a compound command carrying
//! redirections of its own are forms no session performs.
//!
//! # A session that runs in a MACHINE's tree
//!
//! One kind of caller-supplied program is the program of a MACHINE the emulator holds — an
//! instance's boot script — and such a program's whole purpose is to read and write that
//! machine: it keeps a counter on a volume, writes a line to a console device and reads an
//! identity the kernel published to it. A caller that holds the machine hands the session
//! that machine's tree ([`ShellSession::in_volume`], [`super::posix_shell_volume`]) — which
//! then replaces the session's own, because the program's files are the machine's, not this
//! session's — and a session that HAS one performs the forms a program inside a machine is
//! written in, over that tree and nothing else:
//!
//! * the file utilities ([`VOLUME_COMMANDS`]) beside the session-local catalog, which name
//!   what the tree holds as well as change it;
//! * the redirections that reach the tree's other descriptors: standard error to a file or a
//!   device, and standard input from one;
//! * arithmetic expansion ([`super::posix_shell_arithmetic`]), which is how such a program
//!   computes what it writes — performed by every session that works over a tree.
//!
//! None of that reaches a host path, a process or a socket: the tree is in memory, every
//! entry of it was put there by the service that owns the machine, and a path it does not
//! hold resolves to nothing. A session constructed WITHOUT one performs none of those forms,
//! and every other redirection form (another file descriptor, a duplication, a
//! here-document) is one no session performs.
//!
//! # The programs the ENVIRONMENT provides
//!
//! A shell's own utilities are not the only commands a program names: the rest are programs
//! the environment the session runs in has installed, and which ones those are is the
//! caller's fact, not this module's — a build environment installs what its build
//! specification selected, and a session with nothing installed provides nothing beyond the
//! catalog. A caller that has such programs hands this session an
//! [`EnvironmentPrograms`], which is asked for a command the catalog does not name and
//! answers either the status and output of the program it provides for it, or `None` for a
//! name it provides none for — and `None` is the same closed failure as before. Nothing here
//! resolves a command against a host `PATH`, and an [`EnvironmentPrograms`] is handed no way
//! to reach a process, a file or the host: it answers out of what its own caller holds, and
//! the FILES of this session's own tree ([`ProgramFiles`]), which is what a program whose
//! work is to carry a file somewhere reads and writes — still the one tree above, where a
//! path is a key and no host path is reachable.
//!
//! # Pipelines, and the filters that read what one wrote
//!
//! A pipeline CONNECTS its commands: "the standard output of the first command shall be
//! connected to the standard input of the second command", so what the last command of a
//! pipeline wrote is what the pipeline wrote, and every stage before it wrote to the stage
//! after it rather than to the session. That is what makes a program's own computation
//! observable: the bytes one command produced are the bytes the next one counts, digests or
//! selects from. A stage that redirects its own standard output sends it to the file instead,
//! as the shell does, and so passes nothing on; what one stage may pass to the next is bounded
//! by [`Bounds::substitution_bytes`], and a stage past that bound ends the pipeline rather than
//! leaving the stage after it to answer for a stream nothing produced.
//!
//! The commands that READ that input are the text filters ([`TEXT_FILTER_COMMANDS`], whose
//! catalog, options and output forms are stated in
//! [`super::posix_shell_text_filters`]) and `cat` with no file operand: each answers out of ONE
//! input — the file it was given, the stage before it, or the file standard input was
//! redirected from — and writes what it computed over it. A caller whose own ENVIRONMENT
//! provides a program of that name keeps it: the environment is asked first, and the filter is
//! only what a session whose environment provides none falls back to. Nothing else here reads
//! standard input: there is no terminal behind it, so a filter with no input at all is a
//! command that could not run.
//!
//! # Command substitution
//!
//! `$(…)` is a command whose OUTPUT is a word of another command, so EVERY session performs
//! it the way the shell specifies — it is the shell's own form rather than a machine's: the
//! substituted text is read by the same grammar and performed in this session, and what it
//! wrote, with trailing newlines removed, is the word. It is performed in a SUBSHELL: the
//! variables it sets and an `exit` it performs do not reach the session that substituted it,
//! rolled back off a log of what it CHANGED rather than off a copy of the store. Performing
//! one spends a command of the same budget before anything else — so how many a line may open
//! is bounded whatever their text performs — the commands inside it spend their own, what it
//! captures is bounded by [`Bounds::substitution_bytes`], how many one session performs by
//! [`Bounds::substitutions`], and how deeply they may nest by
//! [`Bounds::substitution_depth`]: a substituted command is no wider, and no cheaper, than a
//! command written on the line itself, and the catalogs and the environment's programs are
//! all it can name either.
//!
//! # Bounds
//!
//! Every bound is the emulator's, held in code the program cannot reach, and every one of
//! them is a field of [`Bounds`]: how many commands one run performs (latched — past it a
//! line performs nothing, and performing a command substitution spends one too), how long a
//! line may be and how deeply it may nest (checked WITHOUT parsing, which is what keeps the
//! grammar's own recursion in range), how much one word and one line may expand to, how many
//! variables and how many bytes of them the session holds — which holds for the variables it
//! is HANDED exactly as for the ones its program sets — and, for a caller that KEEPS what the
//! program wrote ([`OutputLimits`]), how many lines and how many bytes per line it may keep.
//!
//! # The services that drive it
//!
//! * `service-com-amazonaws-codebuild-codebuild-20161006`'s `state::build_shell` performs a
//!   buildspec's command lines for an exit status and keeps no output, which is this module
//!   with [`OutputLimits`] absent, and hands it the programs of the runtimes that build
//!   installed as its [`EnvironmentPrograms`];
//! * `service-com-amazonaws-sagemaker-sagemaker`'s `state::notebook_lifecycle_hook` performs
//!   a notebook instance's lifecycle hook and keeps its output, which it publishes to that
//!   hook's CloudWatch Logs stream.
//! * `service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore`'s
//!   `state::code_interpreter_shell` performs a code interpreter session's `executeCommand`
//!   and `startCommandExecution` programs over that session's own workspace, and reports the
//!   two streams separately ([`ShellSession::output`], [`ShellSession::errors`]) because its
//!   API carries a member for each.
//!
//! # The other session, and what the two SHARE
//!
//! `service-com-amazonaws-ecs-amazonec2containerservicev20141113`'s `state::container_shell`
//! performs a container's program as a PROCESS: it loops, waits, installs signal handlers and
//! delivers each line as it is written, which this session does none of — it is driven
//! SYNCHRONOUSLY by callers that perform a whole program while holding their own store's lock
//! (`…lambdamicrovms…::state::guest_machine::in_volume` hands this session a machine's tree
//! from inside that guard, and `…ec2…::state::guest_console` performs a boot script the same
//! way), so the walk over the grammar's tree cannot be the same function in both until those
//! callers no longer need a synchronous one.
//!
//! What IS the same is everything that needs no await, and it lives in ONE place rather than
//! in two copies:
//!
//! * the FILES both sessions work over and the utilities that work over them — the seam, `cat`,
//!   `ls`, `mkdir`, `touch`, `rm`, `sync`, the `test` questions about a file, and the
//!   redirections of the three standard streams ([`super::posix_shell_files`]);
//! * the TEXT FILTERS ([`super::posix_shell_text_filters`]);
//! * the `test` questions about values alone ([`value_test_answer`]);
//! * the in-memory TREE a machine's files are held in ([`super::posix_shell_volume`]).
//!
//! Nothing new should restate any of those — call these modules. A new session needs only its
//! own walk, and only where the one here cannot serve it.

use std::collections::BTreeMap;

use super::posix_shell_arithmetic::{self, ArithmeticVariables};
use super::posix_shell_files::{
    self, AppliedRedirect, FileRefusal, Redirections, ShellFiles,
};
use super::posix_shell_text_filters::{Filter, STANDARD_INPUT_LABEL, TEXT_FILTER_COMMANDS};
use super::posix_shell_volume::ShellVolume;

/// The commands a session provides, as their names.
///
/// Each one's whole effect is inside the session, which is why it can be performed in
/// process: `echo` writes a line of the program's output, `cat` writes a file this session
/// holds, `true`, `false` and `:` answer a status, `exit` ends the session, and
/// `export`/`unset` change its variables. A command that is not one of these is either a
/// TEXT FILTER ([`TEXT_FILTER_COMMANDS`]) or a file utility ([`VOLUME_COMMANDS`]) this
/// session performs, or it needs a program, which is the environment's to provide
/// ([`EnvironmentPrograms`]) and not the shell's, and is answered as a command that could not
/// be run when the environment provides none of that name.
pub const BUILT_IN_COMMANDS: &[&str] = &[
    "echo", "cat", "true", "false", ":", "exit", "export", "unset",
];

/// What one program of the environment a session runs in answered: the status it exited
/// with and the lines it wrote.
#[derive(Debug, Clone)]
pub struct ProgramOutcome {
    /// The status the program exited with, which is the status the command carrying it
    /// answers.
    pub status: i32,
    /// The lines the program wrote, in the order it wrote them.
    pub output: Vec<String>,
}

/// The programs the ENVIRONMENT a session runs in provides, beyond the shell's own
/// utilities ([`BUILT_IN_COMMANDS`]).
///
/// A session asks this for a command whose name the catalog does not hold. An
/// implementation answers only out of state its own caller holds and out of the FILES of the
/// session's tree it is handed ([`ProgramFiles`]) — there is no process to start, no host
/// `PATH` to resolve against and no file of the host to open here — and answers `None`
/// for every name it provides no program for, which the session then reports as a command
/// it could not run.
pub trait EnvironmentPrograms: ::std::fmt::Debug + Send + Sync {
    /// What this environment's program `name` answers for `arguments`, or `None` when this
    /// environment provides no program called `name` — or performs nothing of that
    /// invocation of it.
    ///
    /// `files` is the tree of the session the command was written in: the files a program
    /// carrying one somewhere reads its source out of and writes its destination into. A
    /// program whose work touches no file leaves it alone.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome>;

    /// [`Self::run`] for a program that reads STANDARD INPUT: `standard_input` is what the
    /// stage before it in a pipeline wrote, or the file an input redirection named, and
    /// `None` when the command was given neither.
    ///
    /// This is the form the session asks, so a program that reads its input (a login taking
    /// a password on standard input, a filter) overrides it; the default performs the
    /// invocation without reading one, which is what a program that ignores standard input
    /// does. A name this answers `None` for leaves the input in place for the utility the
    /// session falls back to.
    fn run_with_input(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let _ = standard_input;
        self.run(name, arguments, files)
    }
}

/// The programs of TWO environments over one session, asked in order.
///
/// What an environment provides is not always held in one place: a machine has the programs
/// its own owner installed in it, and a caller that reaches into that machine may supply the
/// ones only IT can — a program whose answers come from state the machine's owner cannot read.
/// This layers the two without either having to know the other's names: a command is asked of
/// the FIRST catalog, and of the second only where the first provides no program of that name,
/// so the machine's own program wins a name they both carry.
#[derive(Debug)]
pub struct LayeredPrograms {
    /// Asked first: the environment whose own programs these are.
    own: Box<dyn EnvironmentPrograms>,
    /// Asked for a name the first provides nothing for.
    supplied: Box<dyn EnvironmentPrograms>,
}

/// The two catalogs as one, `own` taking precedence ([`LayeredPrograms`]).
#[must_use]
pub fn layered(
    own: Box<dyn EnvironmentPrograms>,
    supplied: Box<dyn EnvironmentPrograms>,
) -> Box<dyn EnvironmentPrograms> {
    Box::new(LayeredPrograms { own, supplied })
}

impl EnvironmentPrograms for LayeredPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        self.own
            .run(name, arguments, files)
            .or_else(|| self.supplied.run(name, arguments, files))
    }

    fn run_with_input(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        self.own
            .run_with_input(name, arguments, standard_input, files)
            .or_else(|| {
                self.supplied
                    .run_with_input(name, arguments, standard_input, files)
            })
    }
}

/// The FILES of the tree a session works over, as the ENVIRONMENT's own programs reach them.
///
/// It is the same tree the session's redirections write and its `cat` reads — a machine's
/// where it was given one, and the session's own otherwise — so a program hands a file the
/// commands around it wrote on, and leaves one they read back. A path is a KEY of that tree:
/// nothing here opens, creates or removes a path of the host.
pub trait ProgramFiles {
    /// What reading `path` answers in that tree, or `None` when it holds no file there.
    fn read(&self, path: &str) -> Option<String>;

    /// Write `contents` at `path`, replacing what the tree holds there.
    ///
    /// `false` is a write that did not happen — a path the tree does not accept, an entry it
    /// will not let a program write, or a write past one of its bounds — which the program
    /// reports as its own failure.
    fn write(&mut self, path: &str, contents: &str) -> bool;

    /// Remove the file at `path`, answering whether there was one to remove — what a program
    /// that MOVES a file does to the one it carried away.
    fn remove(&mut self, path: &str) -> bool;

    /// The names directly under the DIRECTORY `path` names (the tree's root for the empty
    /// path), each once and in path order — what a program that LISTS a directory answers.
    ///
    /// The default is empty, which is what a tree that holds no directory entry of its own
    /// answers for every path: an environment whose programs list a directory is one whose
    /// session implements this.
    fn entries_in(&self, path: &str) -> Vec<String> {
        let _ = path;
        Vec::new()
    }

    /// Make `path` a directory of that tree, with every directory above it, answering
    /// whether the tree now has it — what a program that INSTALLS something has to do for
    /// the directories what it installs ships, so the commands around it can write files
    /// under them.
    ///
    /// The default is `false`, which is "this tree has no directory to make": an environment
    /// whose programs make one is one whose session implements this.
    fn make_directories(&mut self, path: &str) -> bool {
        let _ = path;
        false
    }
}

/// The commands a session whose tree is a MACHINE's — or its own WORKING tree
/// ([`ShellSession::with_file_utilities`]) — provides on top of [`BUILT_IN_COMMANDS`], as
/// their names.
///
/// Each one's whole effect is inside the session and the tree it works over: `printf` writes
/// one line of output, `mkdir`, `touch` and `rm` change the tree, `tee` writes what it reads
/// into the files it names and passes it on, `ls` names what the tree
/// holds, `test` (and its `[`
/// spelling) answers a question about it, `sh` performs a program in this same session —
/// `sh -c PROGRAM` the text it was given and `sh FILE` the script the TREE holds at that
/// path, which is how a machine's init system runs one line and how a script left in the
/// tree is run — `.` (with its bash `source` spelling) performs the commands of a file the
/// tree holds in this same session, which is how a program reads a file of assignments the
/// environment published to it, and `sleep` states the TIME the program took
/// ([`ShellSession::slept`]). A session given neither tree provides none of them: a
/// directory, a removal, a nested program and a file to read commands from are what an
/// environment with storage of its own has, and such a session's tree is only the files its
/// redirections left.
pub const VOLUME_COMMANDS: &[&str] = &[
    "printf", "mkdir", "touch", "rm", TEE_COMMAND, LIST_COMMAND, "test", "[", "sh", "bash", ".",
    "source", "sleep",
];

/// The command of [`VOLUME_COMMANDS`] that writes what it READS into the files it names: a
/// program's own way of putting a document somewhere, beside a redirection.
const TEE_COMMAND: &str = "tee";

/// The one command of [`VOLUME_COMMANDS`] that only READS the tree, and so the one an
/// environment's own program for that name takes precedence over
/// ([`ShellSession::run_built_in`]): a caller whose storage has its own catalog of names
/// answers `ls` out of that catalog.
const LIST_COMMAND: &str = "ls";

/// The commands that only change HOW the command after them is run, and are therefore not
/// commands of their own: `nohup` — "invoke a utility immune to hangups", whose operands are
/// "the utility to be invoked" and its arguments — and `setsid`, which runs one in a session
/// of its own. This session has no signal to block and no session to lead, so the command
/// such a wrapper names IS the command performed, with the wrapper's remaining words as its
/// own ([`ShellSession::run_simple_command`]). A wrapper naming no utility runs nothing.
const COMMAND_WRAPPERS: &[&str] = &["nohup", "setsid"];

/// The commands an assignment written before them persists in the session for: POSIX's
/// special built-in utilities, for which "variable assignments specified with special
/// built-in utilities remain in effect after the built-in completes". For every other command
/// an assignment before it is that command's environment alone.
const PERSISTING_COMMANDS: &[&str] = &[":", ".", "exit", "export", "unset"];

/// The statuses a session answers with, so a service whose API reports a status reports the
/// one a shell answers for its own condition.
#[derive(Debug, Clone, Copy)]
pub struct Statuses {
    /// The status of a command that ran to success.
    pub success: i32,
    /// The status of a command that ran and answered failure, which is what the `false`
    /// utility is specified to do ("exit with a status code indicating failure").
    pub failure: i32,
    /// The status a shell reports for a line it could not parse — what `sh` is specified to
    /// exit with on a syntax error it detects.
    pub syntax_error: i32,
    /// The status a shell reports when "the utility to be executed could not be found",
    /// which is what a session answers for every command it provides no program for, for a
    /// command form it does not perform, and for a line past its bounds.
    pub could_not_run: i32,
}

impl Default for Statuses {
    /// The statuses POSIX gives those four conditions.
    fn default() -> Self {
        Self {
            success: 0,
            failure: 1,
            syntax_error: 2,
            could_not_run: 127,
        }
    }
}

/// How much one session may do. Every field is the EMULATOR'S number, not a modelled one:
/// the programs are caller-supplied text, so what one of them can cost is bounded here.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// How many commands one session performs before it is given up on. Latched: past it a
    /// line performs nothing at all.
    pub commands: usize,
    /// The longest line a session reads.
    pub command_bytes: usize,
    /// How many grouping indicators or reserved words one line may carry before the session
    /// gives up on reading it — what keeps the grammar's own recursion in range, since a
    /// parser descends once per nested group.
    pub nesting: usize,
    /// How many bytes one word's expansion may produce.
    pub expansion_bytes: usize,
    /// How many bytes ONE line's expansions may materialise at once, which is their SUM.
    pub line_expansion_bytes: usize,
    /// How many bytes of variables one session may hold at once.
    pub session_bytes: usize,
    /// How many variables one session may hold at once, so neither a few long values nor
    /// very many short ones grow the store without a stated ceiling.
    pub session_variables: usize,
    /// How deeply a word's expansion is walked, applied to the pieces a quoted word nests.
    pub word_depth: usize,
    /// How many command substitutions and `sh -c` programs one session performs, which is
    /// what bounds the work a program can reach by nesting one inside another. Each one
    /// also spends a command of [`Bounds::commands`].
    pub substitutions: usize,
    /// How deeply those substitutions and nested `sh -c` programs may nest, which is what
    /// keeps this module's own recursion in range.
    pub substitution_depth: usize,
    /// How many bytes ONE substitution may capture before it has no value at all, which is
    /// also what ONE stage of a pipeline may pass to the stage after it.
    pub substitution_bytes: usize,
    /// How many files one session's own tree holds at once.
    pub files: usize,
    /// How many bytes ONE of those files may hold, which is also how much of a redirected
    /// command's output is collected for it.
    pub file_bytes: usize,
    /// How many bytes the whole tree may hold at once, so neither a few large files nor very
    /// many small ones grow it without a stated ceiling.
    pub tree_bytes: usize,
    /// How many seconds of `sleep` one session may state in total ([`ShellSession::slept`]),
    /// so a caller that honours that time is never asked for an unbounded wait.
    pub sleep_seconds: u64,
}

impl Default for Bounds {
    /// Bounds far above any real program of the kinds these services carry, and far below
    /// what would cost the host anything noticeable.
    fn default() -> Self {
        Self {
            commands: 1_000,
            command_bytes: 16 * 1024,
            nesting: 64,
            expansion_bytes: 64 * 1024,
            line_expansion_bytes: 256 * 1024,
            session_bytes: 1024 * 1024,
            session_variables: 4_096,
            word_depth: 32,
            substitutions: 256,
            substitution_depth: 8,
            substitution_bytes: 64 * 1024,
            files: 256,
            file_bytes: 256 * 1024,
            tree_bytes: 4 * 1024 * 1024,
            sleep_seconds: 300,
        }
    }
}

/// How much of what a program WROTE a session keeps, for a caller that reports it.
///
/// A caller that reports no output leaves this absent, and then nothing is kept at all: the
/// bytes a command writes are not delivered anywhere, so a session that is not asked for
/// them does not grow with them.
#[derive(Debug, Clone, Copy)]
pub struct OutputLimits {
    /// How many lines one session keeps. Past it a line is not kept at all.
    pub lines: usize,
    /// How many bytes ONE kept line may carry. A longer line is TRUNCATED on a character
    /// boundary rather than dropped, so what the program wrote is still reported.
    pub line_bytes: usize,
}

/// What a session is constructed with: its statuses, its bounds, and whether it keeps what
/// the program wrote.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionConfig {
    /// See [`Statuses`].
    pub statuses: Statuses,
    /// See [`Bounds`].
    pub bounds: Bounds,
    /// See [`OutputLimits`]; `None` keeps no output.
    pub output: Option<OutputLimits>,
}

/// The characters that open a nested group in a line, counted by [`within_parse_bounds`]: a
/// subshell or command substitution, a brace group or expansion, a bracket expression, and
/// the older command-substitution spelling.
const NESTING_INDICATORS: [char; 4] = ['(', '{', '[', '`'];

/// The reserved WORDS that open or close a nested command in a line, counted by
/// [`within_parse_bounds`] beside [`NESTING_INDICATORS`]: the shell's compound commands nest
/// through these rather than through any character, so counting characters alone would leave
/// the grammar's recursion unbounded for a line of `if … then …`. The words that CLOSE one
/// are counted too, which can only make the count higher — an over-count refuses a line and
/// never admits one.
const NESTING_WORDS: [&str; 16] = [
    "if", "then", "elif", "else", "fi", "while", "until", "for", "select", "case", "esac", "in",
    "do", "done", "function", "time",
];

/// The characters a reserved word may be glued to in a line: the shell's own separators and
/// operators. A word is counted when it stands between any of these (or whitespace), so
/// `:;if` carries an `if` exactly as `: ; if` does.
const WORD_SEPARATORS: [char; 10] = [';', '&', '|', '(', ')', '{', '}', '<', '>', '!'];

/// The path a session's tree keys a file by, for the path a program named — `None` when the
/// program named no path at all.
///
/// A relative path is what a program's commands write, and `./name` and `name` are the same
/// file to them, so the one spelling that differs only in that prefix is folded into it.
/// Nothing else about the path is interpreted: it is a key in this session's own tree, never
/// a path on the host.
fn file_path(named: &str) -> Option<String> {
    let trimmed = named.trim();
    let folded = trimmed.strip_prefix("./").unwrap_or(trimmed);
    (!folded.is_empty()).then(|| folded.to_string())
}

/// Whether `line` is inside the bounds a parse is given: its size, and how many nested
/// commands it opens — through a grouping character or a reserved word.
///
/// Checked WITHOUT parsing, which is the point: the parser is what the nesting bound
/// protects.
fn within_parse_bounds(line: &str, bounds: &Bounds) -> bool {
    if line.len() > bounds.command_bytes {
        return false;
    }
    let grouped = line
        .chars()
        .filter(|character| NESTING_INDICATORS.contains(character))
        .count();
    let worded = line
        .split(|character: char| character.is_whitespace() || WORD_SEPARATORS.contains(&character))
        .filter(|word| NESTING_WORDS.contains(word))
        .count();
    grouped.saturating_add(worded) <= bounds.nesting
}

/// What one command SUBSTITUTION, or one redirected command, has captured so far.
#[derive(Debug, Default)]
struct Capture {
    /// The bytes the commands inside it wrote to standard output.
    text: String,
    /// How many of those bytes it may hold: what a file may hold for a redirected command
    /// ([`Bounds::file_bytes`]), and what a substitution may be worth
    /// ([`Bounds::substitution_bytes`]).
    limit: usize,
    /// Set once it went past that limit, in which case the write did not happen at all and
    /// the substitution has no value rather than a clipped one.
    overflowed: bool,
}

/// The session one caller-supplied program runs in: the variables its commands see and
/// change, what its `echo` commands wrote (when the caller keeps output), the files its
/// redirections left behind, and how much running is left in its budget.
#[derive(Debug)]
pub struct ShellSession {
    config: SessionConfig,
    /// The variables this session holds, which is what a word's parameter expansion resolves
    /// against and what a caller reads an exported variable from.
    variables: BTreeMap<String, String>,
    /// How many bytes those variables' names and values come to, kept as they are written
    /// rather than recomputed, so the session-bytes check costs one addition however many
    /// variables the session holds.
    held_bytes: usize,
    /// How many more bytes the line being run may expand to, reset for each line.
    expansion_left: usize,
    /// How many more commands this session may perform.
    commands_left: usize,
    /// The status `exit` ended this session with, once a command asked for one: every later
    /// line is then not performed at all, the way a shell stops at `exit`.
    exit_status: Option<i32>,
    /// What the program wrote, one entry per performed command that wrote a line, kept only
    /// when the configuration asks for it.
    output: Vec<String>,
    /// What the program's commands reported they could not do, one entry per line, kept
    /// under the same limits as [`Self::output`] and for the same callers: a service whose
    /// API reports the two streams SEPARATELY ([`Self::errors`]) reads this one, and nothing
    /// is kept at all for a session whose configuration asks for no output.
    errors: Vec<String>,
    /// The programs the environment this session runs in provides, asked for a command
    /// neither catalog names; `None` for a session whose environment provides none.
    programs: Option<Box<dyn EnvironmentPrograms>>,
    /// The tree this session's commands read and write when it runs in a MACHINE's one
    /// ([`Self::in_volume`]) — `None` for a session whose tree is its own
    /// ([`Self::files`]), which performs neither the file utilities nor the forms that reach
    /// a machine's other descriptors.
    volume: Option<ShellVolume>,
    /// The command substitutions being captured, innermost last: while one is open, what a
    /// command writes to standard output is ITS value rather than the session's output.
    captures: Vec<Capture>,
    /// For each command substitution being performed, innermost last, what the session's
    /// variable store has to be put back to when it ends: the value it held for each name the
    /// substituted commands touched, before they did. A substitution is a SUBSHELL, and this
    /// is what rolls one back — which costs what it changed rather than a copy of the store.
    substitution_undo: Vec<BTreeMap<String, Option<String>>>,
    /// The status of the last command substitution performed for the simple command being
    /// run, which is the status of a command that has no command name at all.
    last_substitution_status: Option<i32>,
    /// How many more substitutions and `sh -c` programs this session performs.
    substitutions_left: usize,
    /// How many `sh -c` programs are running inside one another right now, counted beside
    /// the captures above as the DEPTH this module's own recursion is bounded by.
    nested_programs: usize,
    /// The files this session's own tree holds, keyed by the path a redirection named — the
    /// tree of a session that was given no machine's.
    files: BTreeMap<String, String>,
    /// Set for a session whose own tree is the WORKING tree of the environment its program
    /// runs in ([`Self::with_file_utilities`]), which is what makes the file utilities
    /// ([`VOLUME_COMMANDS`]) commands this session performs over that tree.
    working_tree: bool,
    /// How many bytes that tree comes to, kept as it is written.
    tree_bytes: usize,
    /// Set when a write to the file a redirection named did not happen, which is what makes
    /// the command carrying it answer as one that could not be run.
    redirect_failed: bool,
    /// What the stage BEFORE the command being performed wrote, which is that command's
    /// standard input — `None` for a command that is not reading one stage of a pipeline.
    /// Held by [`ShellSession::run_pipeline`] for the one command it belongs to, and taken by
    /// the first command that reads it, the way a stream is consumed by whoever reads it.
    piped_input: Option<String>,
    /// How long this session's `sleep` commands have stated the program took.
    slept: ::std::time::Duration,
}

/// The FILES of the tree this session works over, handed to the ENVIRONMENT's programs
/// while one of them runs ([`ShellSession::run_environment_program`]).
///
/// It reaches the one tree the session's own commands read and write ([`ShellSession::read_path`],
/// [`ShellSession::write_path`]) and nothing else of the session.
struct SessionFiles<'a> {
    session: &'a mut ShellSession,
}

impl ProgramFiles for SessionFiles<'_> {
    fn read(&self, path: &str) -> Option<String> {
        self.session.read_path(path)
    }

    fn write(&mut self, path: &str, contents: &str) -> bool {
        self.session.write_path(path, contents, false)
    }

    fn remove(&mut self, path: &str) -> bool {
        self.session.tree_remove_file(path)
    }

    fn entries_in(&self, path: &str) -> Vec<String> {
        self.session.tree_entry_names(path)
    }

    fn make_directories(&mut self, path: &str) -> bool {
        self.session.tree_make_directories(path)
    }
}

/// The same files, as the SEAM the shared file utilities reach them through
/// ([`posix_shell_files::ShellFiles`]) — so what `cat`, `ls`, `mkdir`, `touch` and `rm` do and
/// SAY is written once for every session in this workspace, and what differs is only where
/// the files are.
///
/// A refusal is reported as the error the tree's answer stands for: this tree answers a write
/// or a removal with whether it happened, and a path it will not take one for is the
/// `No such file or directory` a session has always reported for it. It keeps no mode, owner
/// or modification time, so it reports no [`posix_shell_files::FileEntry`] — which is what
/// makes a long listing a form these sessions do not perform.
impl ShellFiles for SessionFiles<'_> {
    fn read(&self, path: &str) -> Result<String, FileRefusal> {
        self.session.read_path(path).ok_or(FileRefusal::NotFound)
    }

    fn write(&mut self, path: &str, text: &str, append: bool) -> Result<(), FileRefusal> {
        if self.session.write_path(path, text, append) {
            Ok(())
        } else {
            Err(FileRefusal::NotFound)
        }
    }

    fn make_directories(&mut self, path: &str) -> Result<(), FileRefusal> {
        if self.session.tree_make_directories(path) {
            Ok(())
        } else {
            Err(FileRefusal::NotFound)
        }
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FileRefusal> {
        if self.session.tree_holds_directory(path) {
            return Err(FileRefusal::IsADirectory);
        }
        if self.session.tree_remove_file(path) {
            Ok(())
        } else {
            Err(FileRefusal::NotFound)
        }
    }

    fn exists(&self, path: &str) -> bool {
        self.session.tree_holds(path)
    }

    fn is_file(&self, path: &str) -> bool {
        self.session.tree_holds_file(path)
    }

    fn is_directory(&self, path: &str) -> bool {
        self.session.tree_holds_directory(path)
    }

    fn writable(&self, path: &str) -> bool {
        // This tree takes a write to any path it accepts at all, so what `-w` asks about is
        // whether it holds it.
        self.session.tree_holds(path)
    }

    fn entry_names(&self, path: &str) -> Vec<String> {
        self.session.tree_entry_names(path)
    }

    fn keeps_directories(&self) -> bool {
        // A session's OWN tree keys a file by its whole path and holds no directory entry, so
        // every directory above a file exists already; a MACHINE's tree holds them.
        self.session.volume.is_some()
    }
}

impl ShellSession {
    /// A session holding `variables`, with the whole command budget `config` allows left,
    /// whose environment provides no program beyond the catalog.
    ///
    /// The store's ceilings ([`Bounds::session_variables`], [`Bounds::session_bytes`]) hold
    /// for the variables a session STARTS with exactly as they do for the ones its program
    /// sets, so a caller handing it more than a session may hold hands it what fits: the rest
    /// are not in the environment the program is given.
    #[must_use]
    pub fn new(variables: BTreeMap<String, String>, config: SessionConfig) -> Self {
        let mut session = Self {
            variables: BTreeMap::new(),
            held_bytes: 0,
            expansion_left: config.bounds.line_expansion_bytes,
            commands_left: config.bounds.commands,
            exit_status: None,
            output: Vec::new(),
            errors: Vec::new(),
            programs: None,
            volume: None,
            captures: Vec::new(),
            substitution_undo: Vec::new(),
            last_substitution_status: None,
            substitutions_left: config.bounds.substitutions,
            nested_programs: 0,
            files: BTreeMap::new(),
            working_tree: false,
            tree_bytes: 0,
            redirect_failed: false,
            piped_input: None,
            slept: ::std::time::Duration::ZERO,
            config,
        };
        for (name, value) in variables {
            if !session.set_variable(name, value) {
                break;
            }
        }
        session
    }

    /// [`Self::new`] for a session whose environment provides `programs` as well: a command
    /// the catalog does not name is asked of them before it is reported as one this session
    /// could not run.
    #[must_use]
    pub fn with_programs(
        variables: BTreeMap<String, String>,
        config: SessionConfig,
        programs: Box<dyn EnvironmentPrograms>,
    ) -> Self {
        Self {
            programs: Some(programs),
            ..Self::new(variables, config)
        }
    }

    /// A session holding `variables` that runs IN `volume`: a MACHINE's tree, which replaces
    /// the session's own as what its commands read and write, and the whole of what they can
    /// reach. See the module docs for the forms a session with one performs.
    #[must_use]
    pub fn in_volume(
        variables: BTreeMap<String, String>,
        config: SessionConfig,
        volume: ShellVolume,
    ) -> Self {
        Self {
            volume: Some(volume),
            ..Self::new(variables, config)
        }
    }

    /// The tree this session ran in, with everything its program wrote to it — `None` for a
    /// session that was given none. The caller keeps what it holds beside the machine, so
    /// the next run of a program on that machine reads what this one wrote.
    #[must_use]
    pub fn into_volume(self) -> Option<ShellVolume> {
        self.volume
    }

    /// This session with its OWN tree declared the WORKING tree of the environment the
    /// program runs in, so the file utilities ([`VOLUME_COMMANDS`]) are commands it performs
    /// over that tree.
    ///
    /// What a caller declares with this is that its environment HAS storage of its own — a
    /// build container's source directory, say — rather than only the files a redirection
    /// leaves behind. That tree keys a file by the whole path the program named and holds no
    /// directory entry of its own, so a directory of it exists exactly as long as a file
    /// under it does: `mkdir` has nothing to make and answers success (it fails only where a
    /// FILE already holds that path), `test -d` answers for a path some file lies under, and
    /// a write never needs the directory above it to have been made first. The forms that
    /// reach a MACHINE's other descriptors are not these: they stay a session given a
    /// machine's tree ([`Self::in_volume`]).
    #[must_use]
    pub fn with_file_utilities(mut self) -> Self {
        self.working_tree = true;
        self
    }

    /// This session with `programs` as the programs the ENVIRONMENT it runs in provides, for
    /// a caller that constructed it some other way than [`Self::with_programs`] — a session
    /// running in a MACHINE's tree ([`Self::in_volume`]) whose machine also has programs
    /// installed in it.
    ///
    /// The same seam [`Self::with_programs`] fills: a command neither catalog names is asked
    /// of `programs` before it is reported as one this session could not run.
    #[must_use]
    pub fn with_environment_programs(mut self, programs: Box<dyn EnvironmentPrograms>) -> Self {
        self.programs = Some(programs);
        self
    }

    /// Put `contents` at `path` in the tree this session reads and writes, the way the
    /// environment seeds a file its program is to find there — a source archive the build
    /// unpacked, a cached path it restored.
    ///
    /// `false` is a seed that did not happen, under [`Self::write_path`]'s rule: a path the
    /// tree does not accept, or a write past one of the tree's bounds.
    pub fn put_file(&mut self, path: &str, contents: &str) -> bool {
        self.write_path(path, contents, false)
    }

    /// The files the tree this session works over holds, as `(path, contents)` in path
    /// order — what the caller takes back out of a finished run: the files a build's
    /// commands produced, which are what its artifacts and its cache are selected from.
    #[must_use]
    pub fn tree_files(&self) -> Vec<(&str, &str)> {
        match &self.volume {
            Some(volume) => volume.stored_files().collect(),
            None => self
                .files
                .iter()
                .map(|(path, contents)| (path.as_str(), contents.as_str()))
                .collect(),
        }
    }

    /// Forget that a program ended this session with `exit`, so the next line runs.
    ///
    /// For a caller that hands this session SEVERAL programs over one tree — a phase's
    /// command list and then the block that runs after it — where an `exit` ends the program
    /// that performed it and not the ones the caller runs next.
    pub fn clear_exit(&mut self) {
        self.exit_status = None;
    }

    /// The tree this session runs in, for a caller reading what the program has written to
    /// it so far.
    #[must_use]
    pub fn volume(&self) -> Option<&ShellVolume> {
        self.volume.as_ref()
    }

    /// The value this session holds for `name`, or `None` when it holds none.
    #[must_use]
    pub fn variable(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(String::as_str)
    }

    /// The status `exit` ended this session with, or `None` while nothing has.
    #[must_use]
    pub fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }

    /// The lines this session's `echo` commands wrote, in the order they wrote them. Empty
    /// for a session whose configuration keeps no output.
    #[must_use]
    pub fn output(&self) -> &[String] {
        &self.output
    }

    /// [`Self::output`], taking the lines rather than borrowing them.
    #[must_use]
    pub fn into_output(self) -> Vec<String> {
        self.output
    }

    /// The lines this session's commands wrote to standard ERROR, in the order they wrote
    /// them — what a command reported it could not do.
    ///
    /// For a caller whose API reports the two streams as members of its own: a session given
    /// a MACHINE's tree keeps a diagnostic on its console ([`Self::output`]) as well, because
    /// that is where a guest's diagnostics are read, and for every other session this is the
    /// only place one is kept. Empty for a session whose configuration keeps no output, and
    /// for one whose commands reported nothing.
    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// How long this session's `sleep` commands stated the program took
    /// ([`Self::run_sleep`]).
    ///
    /// No thread waited for it: a caller whose API reports how long a command took, or
    /// converges a submitted task on it, is the one that honours this.
    #[must_use]
    pub fn slept(&self) -> ::std::time::Duration {
        self.slept
    }


    /// Perform one LINE of the program and answer the status the shell answered for it.
    ///
    /// A line that does not parse is the shell's own syntax error
    /// ([`Statuses::syntax_error`]); a line outside the bounds this parse is given
    /// ([`within_parse_bounds`]), or one reached after the command budget is spent, is a
    /// line this session did not perform at all, which is reported as a command it could
    /// not run ([`Statuses::could_not_run`]) rather than as one that succeeded.
    pub fn run(&mut self, line: &str) -> i32 {
        // Each line gets the whole expansion budget: the words of one line are live at the
        // same instant, and the words of the line before it are not.
        self.expansion_left = self.config.bounds.line_expansion_bytes;
        self.run_program(line)
    }

    /// [`Self::run`] without resetting the line's expansion budget: what a command
    /// substitution and an `sh -c` program are performed through, so the whole of one line
    /// — the commands nested inside it included — shares the one budget.
    fn run_program(&mut self, line: &str) -> i32 {
        if !within_parse_bounds(line, &self.config.bounds) || self.commands_left == 0 {
            return self.config.statuses.could_not_run;
        }
        let Ok(tokens) = ::brush_parser::tokenize_str(line) else {
            return self.config.statuses.syntax_error;
        };
        let options = ::brush_parser::ParserOptions::default();
        let Ok(program) = ::brush_parser::parse_tokens(&tokens, &options) else {
            return self.config.statuses.syntax_error;
        };
        let mut status = self.config.statuses.success;
        for complete_command in &program.complete_commands {
            for item in &complete_command.0 {
                status = self.run_and_or_list(&item.0);
                if self.exit_status.is_some() {
                    return status;
                }
            }
        }
        status
    }

    /// Perform an and-or list: its first pipeline, then each later pipeline only when the
    /// operator before it says so — `&&` runs the next on a zero status, `||` on a non-zero
    /// one — and answer the status of the last pipeline that ran.
    fn run_and_or_list(&mut self, list: &::brush_parser::ast::AndOrList) -> i32 {
        let mut status = self.run_pipeline(&list.first);
        for next in &list.additional {
            if self.exit_status.is_some() {
                break;
            }
            match next {
                ::brush_parser::ast::AndOr::And(pipeline) => {
                    if status == self.config.statuses.success {
                        status = self.run_pipeline(pipeline);
                    }
                }
                ::brush_parser::ast::AndOr::Or(pipeline) => {
                    if status != self.config.statuses.success {
                        status = self.run_pipeline(pipeline);
                    }
                }
            }
        }
        status
    }

    /// Perform a pipeline and answer its status: "the exit status of a pipeline shall be the
    /// exit status of the last command", negated when the pipeline is written with `!`.
    ///
    /// Every command of the pipeline is performed, in order, because each answers its own
    /// status, and each is CONNECTED to the one after it: what a stage writes to standard
    /// output is the standard input of the next stage ([`Self::standard_input`]) rather than
    /// what the session keeps, so only the LAST stage's output is the pipeline's. A stage
    /// whose own standard output is redirected writes to that file and passes nothing on, the
    /// way the shell's redirection takes precedence over the pipe.
    ///
    /// A stage that wrote more than [`Bounds::substitution_bytes`] ends the pipeline: the bytes
    /// the pipe could not carry are bytes the stage after it would have read, so the pipeline
    /// answers the failure of the write that did not happen rather than running the rest of it
    /// over an empty stream — a stage past the bound must not be able to make a later stage
    /// answer for input nothing produced.
    fn run_pipeline(&mut self, pipeline: &::brush_parser::ast::Pipeline) -> i32 {
        let mut status = self.config.statuses.success;
        let last = pipeline.seq.len().saturating_sub(1);
        // What the stage before the one being performed wrote.
        let mut upstream: Option<String> = None;
        for (index, command) in pipeline.seq.iter().enumerate() {
            // Every stage but the last writes to the stage after it, which is what collects
            // its output here instead of the session.
            let connected = index < last;
            if connected {
                self.captures.push(Capture {
                    limit: self.config.bounds.substitution_bytes,
                    ..Capture::default()
                });
            }
            // The input belongs to this one command: whatever the command around this
            // pipeline was reading is put back after it.
            let held_input = ::std::mem::replace(&mut self.piped_input, upstream.take());
            status = self.run_command(command);
            self.piped_input = held_input;
            if connected {
                let captured = self.captures.pop().unwrap_or_default();
                if captured.overflowed {
                    // Not a pipeline that ran, so not one a `!` negates either.
                    return self.config.statuses.failure;
                }
                upstream = Some(captured.text);
            }
            if self.exit_status.is_some() {
                break;
            }
        }
        if pipeline.bang {
            status = if status == self.config.statuses.success {
                self.config.statuses.failure
            } else {
                self.config.statuses.success
            };
        }
        status
    }

    /// Perform one command of a pipeline.
    ///
    /// A SIMPLE command is the form every session performs. The grammar's COMPOUND
    /// commands — the control flow a program is written with — are performed over this
    /// same catalog by a session that works over a tree ([`Self::run_compound`]), because
    /// they need no primitive beyond the commands inside them. Every other form a shell
    /// grammar admits — a function definition, an extended test, a compound command
    /// carrying redirections of its own — is one this session does not perform, and a
    /// command that did not run answers as one that could not be run rather than as one
    /// that succeeded.
    ///
    /// EVERY command spends one of [`Bounds::commands`] here, whatever form it takes and
    /// whether or not this session performs it: that budget is latched, so it is what ends
    /// a loop with no end of its own, and a form that answered without running would
    /// otherwise let a loop iterate for nothing.
    fn run_command(&mut self, command: &::brush_parser::ast::Command) -> i32 {
        if self.commands_left == 0 {
            return self.config.statuses.could_not_run;
        }
        self.commands_left = self.commands_left.saturating_sub(1);
        match command {
            ::brush_parser::ast::Command::Simple(simple) => self.run_simple_command(simple),
            ::brush_parser::ast::Command::Compound(compound, redirections)
                if redirections.is_none() && self.works_over_a_tree() =>
            {
                self.run_compound(compound)
            }
            _ => self.config.statuses.could_not_run,
        }
    }

    /// Whether this session works over a TREE — a machine's ([`Self::in_volume`]) or its
    /// own working one ([`Self::with_file_utilities`]) — which is what makes its caller's
    /// program a script of an environment with storage rather than one command line.
    fn works_over_a_tree(&self) -> bool {
        self.volume.is_some() || self.working_tree
    }

    /// Perform a COMPOUND command: the control flow the grammar reads, over the commands
    /// inside it.
    ///
    /// `if`/`elif`/`else` runs the branch whose condition answered success, `while` and
    /// `until` repeat their body while their condition answers success (or failure), and a
    /// brace group runs its list in this session. Every command inside — this compound
    /// command itself included ([`Self::run_command`]) — spends the session's own command
    /// budget, which is what bounds a loop: each iteration performs at least its condition,
    /// so once the budget is gone the loop answers as a command that could not run rather
    /// than iterating again.
    ///
    /// Every other compound form is one this session does not perform: a subshell and a
    /// `case` clause need an environment of their own or a pattern evaluation this session
    /// has none of, so they answer as a command that could not be run.
    fn run_compound(&mut self, compound: &::brush_parser::ast::CompoundCommand) -> i32 {
        use ::brush_parser::ast::CompoundCommand as Compound;
        match compound {
            Compound::BraceGroup(group) => self.run_compound_list(&group.list),
            Compound::IfClause(clause) => self.run_if_clause(clause),
            Compound::WhileClause(clause) => self.run_while_clause(clause, true),
            Compound::UntilClause(clause) => self.run_while_clause(clause, false),
            Compound::ForClause(clause) => self.run_for_clause(clause),
            _ => self.config.statuses.could_not_run,
        }
    }

    /// Perform a list of and-or lists — the body of a compound command — and answer the
    /// status of the last one that ran.
    fn run_compound_list(&mut self, list: &::brush_parser::ast::CompoundList) -> i32 {
        let mut status = self.config.statuses.success;
        for item in &list.0 {
            status = self.run_and_or_list(&item.0);
            if self.exit_status.is_some() {
                return status;
            }
        }
        status
    }

    /// `if LIST; then LIST; elif LIST; then LIST; else LIST; fi`: the branch whose
    /// condition answered success, and success for an `if` no branch of which ran, which is
    /// the status POSIX gives it ("exit status of zero if no condition is true").
    fn run_if_clause(&mut self, clause: &::brush_parser::ast::IfClauseCommand) -> i32 {
        let condition = self.run_compound_list(&clause.condition);
        if self.exit_status.is_some() {
            return condition;
        }
        if condition == self.config.statuses.success {
            return self.run_compound_list(&clause.then);
        }
        for otherwise in clause.elses.iter().flatten() {
            match otherwise.condition.as_ref() {
                // An `elif`, which is its own condition and its own branch.
                Some(condition) => {
                    let answered = self.run_compound_list(condition);
                    if self.exit_status.is_some() {
                        return answered;
                    }
                    if answered == self.config.statuses.success {
                        return self.run_compound_list(&otherwise.body);
                    }
                }
                // The `else` branch, which runs when no condition before it did.
                None => return self.run_compound_list(&otherwise.body),
            }
        }
        self.config.statuses.success
    }

    /// `while LIST; do LIST; done`, and `until`, which is the same with the condition read
    /// the other way round. The status is the last body that ran, and success for a loop
    /// whose body never did.
    fn run_while_clause(
        &mut self,
        clause: &::brush_parser::ast::WhileOrUntilClauseCommand,
        while_success: bool,
    ) -> i32 {
        let mut status = self.config.statuses.success;
        loop {
            // The budget is what ends a loop that has no end of its own, and a loop cut
            // off by it did not run to its own end: it answers as a command that could
            // not run rather than as the body that last succeeded.
            if self.commands_left == 0 {
                return self.config.statuses.could_not_run;
            }
            let condition = self.run_compound_list(&clause.0);
            if self.exit_status.is_some() {
                return condition;
            }
            if (condition == self.config.statuses.success) != while_success {
                return status;
            }
            status = self.run_compound_list(&clause.1.list);
            if self.exit_status.is_some() {
                return status;
            }
        }
    }

    /// `for NAME in WORD…; do LIST; done`: the body once per word, with the loop's variable
    /// holding that word. A `for` with no word list at all iterates over nothing here —
    /// there are no positional parameters for it to take.
    fn run_for_clause(&mut self, clause: &::brush_parser::ast::ForClauseCommand) -> i32 {
        let mut status = self.config.statuses.success;
        let Some(values) = clause.values.as_ref() else {
            return status;
        };
        for word in values {
            let Some(value) = self.expand_word(&word.value) else {
                return self.config.statuses.could_not_run;
            };
            if !self.set_variable(clause.variable_name.clone(), value) {
                return self.config.statuses.could_not_run;
            }
            if self.commands_left == 0 {
                return self.config.statuses.could_not_run;
            }
            status = self.run_compound_list(&clause.body.list);
            if self.exit_status.is_some() {
                return status;
            }
        }
        status
    }

    /// Perform a simple command: its variable assignments and its redirections, then the
    /// command its first word names with the words after it as its arguments, with what it
    /// writes going to the file a redirection names.
    ///
    /// A command with no word at all is the assignment form — "the variable assignments
    /// shall affect the current execution environment" — which is how a program sets a
    /// variable a caller later reads. An assignment written BEFORE a command word affects
    /// that command's environment alone, and persists in the session only for the special
    /// built-in utilities POSIX keeps it for ([`PERSISTING_COMMANDS`]); since every word is
    /// expanded before the assignments are applied, a command that does not keep them sees
    /// the value either way.
    ///
    /// An output redirection writes what the command wrote into the tree this session reads
    /// and writes ([`Self::apply_redirect`]); the other descriptors a machine's program
    /// redirects are performed only by a session that was given a machine's tree. Any other
    /// prefix or suffix item is a redirection form or a substitution this session does not
    /// perform, and the command is then not run.
    ///
    /// The file a redirection names is written whatever the command answered, the way a
    /// shell opens it before it resolves the command at all: a command this session has no
    /// utility for leaves the file it redirected to behind, empty.
    fn run_simple_command(&mut self, simple: &::brush_parser::ast::SimpleCommand) -> i32 {
        // The command budget was spent for this command by [`Self::run_command`], which
        // spends it for every form.
        // The status of a command with no command name is the status of the last command
        // substitution ITS OWN words performed, so what an earlier command's words
        // substituted is not read here.
        self.last_substitution_status = None;
        let mut assignments: Vec<(String, String)> = Vec::new();
        let mut words: Vec<String> = Vec::new();
        let mut redirections = Redirections::default();
        let prefix = simple.prefix.as_ref().map(|prefix| &prefix.0);
        let suffix = simple.suffix.as_ref().map(|suffix| &suffix.0);
        for item in prefix.into_iter().chain(suffix).flatten() {
            match item {
                ::brush_parser::ast::CommandPrefixOrSuffixItem::Word(word) => {
                    let Some(expanded) = self.expand_word(&word.value) else {
                        return self.config.statuses.could_not_run;
                    };
                    words.push(expanded);
                }
                ::brush_parser::ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _) => {
                    let Some(pair) = self.assignment_of(assignment) else {
                        return self.config.statuses.could_not_run;
                    };
                    assignments.push(pair);
                }
                ::brush_parser::ast::CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
                    if !self.apply_redirect(redirect, &mut redirections) {
                        return self.config.statuses.could_not_run;
                    }
                }
                _ => return self.config.statuses.could_not_run,
            }
        }
        let name = match simple.word_or_name.as_ref() {
            Some(word) => match self.expand_word(&word.value) {
                Some(expanded) => Some(expanded),
                None => return self.config.statuses.could_not_run,
            },
            None => None,
        };
        // A command invoked through a WRAPPER that only changes how it is run
        // ([`COMMAND_WRAPPERS`]) is that command: the wrapper's first remaining word is the
        // command name and the rest are its words. One naming no utility at all runs
        // nothing, which is the wrapper's own failure.
        let mut name = name;
        while name
            .as_deref()
            .is_some_and(|name| COMMAND_WRAPPERS.contains(&name))
        {
            if words.is_empty() {
                return self.config.statuses.could_not_run;
            }
            name = Some(words.remove(0));
        }
        let persists = name
            .as_deref()
            .is_none_or(|name| PERSISTING_COMMANDS.contains(&name));
        if persists {
            for (name, value) in assignments {
                if !self.set_variable(name, value) {
                    return self.config.statuses.could_not_run;
                }
            }
        }
        // What a redirected command writes is collected for the file and written when the
        // command has finished, the way a shell's own redirection is one write of what the
        // command produced: a write the tree will not take then leaves the tree as it was.
        //
        // The refused-write flag is this command's own: a command nested inside it (a
        // substitution, an `sh -c` program) answers for its own refused write, and the one
        // held for a command still running is restored after it rather than consumed by the
        // nested one.
        let outer_refused = ::std::mem::replace(&mut self.redirect_failed, false);
        let redirected = redirections.out.take();
        if redirected.is_some() {
            self.captures.push(Capture {
                limit: self.config.bounds.file_bytes,
                ..Capture::default()
            });
        }
        let status = match name {
            Some(name) => self.run_built_in(&name, &words, &redirections),
            // Nothing but assignments, or a redirection alone: they have been applied, which
            // is the whole command. "If there is no command name, but the command contained a
            // command substitution, the command shall complete with the exit status of the
            // last command substitution performed."
            None => self
                .last_substitution_status
                .take()
                .unwrap_or(self.config.statuses.success),
        };
        if let Some((path, append)) = redirected {
            // The file a redirection names is written whatever the command answered, the way
            // a shell opens it before it resolves the command at all: a command this session
            // has no utility for leaves that file behind, empty.
            let captured = self.captures.pop().unwrap_or_default();
            if captured.overflowed || !self.write_path(&path, &captured.text, append) {
                self.redirect_failed = true;
            }
        }
        if ::std::mem::replace(&mut self.redirect_failed, outer_refused) {
            // A write the tree would not take did not happen, so the command did not do what
            // it was asked: reported as one that could not be run rather than as one that
            // wrote the file.
            return self.config.statuses.could_not_run;
        }
        status
    }

    /// One redirection of a simple command, recorded in `redirections` — `false` for a form
    /// this session does not perform.
    ///
    /// WHICH forms are read is the shared reading of a redirection node
    /// ([`AppliedRedirect::of`]); which of them this SESSION performs is its own: every
    /// session performs the output redirections of standard output and the duplication of one
    /// standard stream onto the other, while standard error to a file and standard input from
    /// one are a MACHINE program's forms, so only a session given a machine's tree performs
    /// them.
    ///
    /// A session given a machine's tree also performs the HERE-DOCUMENT and HERE-STRING
    /// forms, whose text is written inside the command itself: `<<WORD` — with `<<-WORD`
    /// stripping the leading tabs off each of its lines — and `<<<WORD` are the command's
    /// standard input, which is how a machine's program writes a document it was given into a
    /// file. A document whose delimiter was QUOTED (`<<'WORD'`) is taken literally, and an
    /// unquoted one is expanded, as the shell specifies.
    fn apply_redirect(
        &mut self,
        redirect: &::brush_parser::ast::IoRedirect,
        redirections: &mut Redirections,
    ) -> bool {
        let machine = self.volume.is_some();
        if let Some(text) = self.here_document(redirect) {
            // A here-document reaches the descriptor a `<` does, so it is a form a session
            // that works over a tree performs.
            if !machine {
                return false;
            }
            redirections.input_text = Some(text);
            return true;
        }
        let ::brush_parser::ast::IoRedirect::File(descriptor, kind, target) = redirect else {
            return false;
        };
        let expanded = match target {
            ::brush_parser::ast::IoFileRedirectTarget::Duplicate(word)
            | ::brush_parser::ast::IoFileRedirectTarget::Filename(word) => {
                match self.expand_word(&word.value) {
                    Some(expanded) => expanded,
                    None => return false,
                }
            }
            _ => return false,
        };
        let Some(applied) = AppliedRedirect::of(*descriptor, kind, &expanded) else {
            return false;
        };
        if !machine
            && matches!(
                applied,
                AppliedRedirect::Input(_) | AppliedRedirect::Error(..)
            )
        {
            return false;
        }
        applied.record(redirections);
        true
    }

    /// The TEXT one here-document or here-string carries, or `None` for a redirection that is
    /// neither.
    ///
    /// `<<WORD` is the document written under the command, `<<-WORD` is the same with the
    /// leading tabs stripped off each of its lines, and `<<<WORD` is the one word itself with
    /// a line ending, which is what the shell gives the command on its standard input.
    /// Whether the text is EXPANDED is the delimiter's own question: a quoted delimiter takes
    /// the document literally, and the parser reports which it was.
    fn here_document(&mut self, redirect: &::brush_parser::ast::IoRedirect) -> Option<String> {
        match redirect {
            ::brush_parser::ast::IoRedirect::HereDocument(_fd, document) => {
                let text = if document.requires_expansion {
                    self.expand_word(&document.doc.value)?
                } else {
                    document.doc.value.clone()
                };
                Some(if document.remove_tabs {
                    text.lines()
                        .map(|line| format!("{}\n", line.trim_start_matches('\t')))
                        .collect()
                } else {
                    text
                })
            }
            ::brush_parser::ast::IoRedirect::HereString(_fd, word) => {
                Some(format!("{}\n", self.expand_word(&word.value)?))
            }
            _ => None,
        }
    }

    /// What reading `path` answers in the tree this session reads: a machine's tree where it
    /// was given one, and its own otherwise.
    fn read_path(&self, path: &str) -> Option<String> {
        match &self.volume {
            Some(volume) => volume.read(path),
            None => self.files.get(&file_path(path)?).cloned(),
        }
    }

    /// What the command being performed reads from STANDARD INPUT, and `None` when there is
    /// none to read.
    ///
    /// The file an input redirection names, when the command carries one (a session given a
    /// MACHINE's tree is the one that performs that form), and otherwise what the stage before
    /// it in a pipeline wrote. It is TAKEN: a stream is consumed by whoever reads it, so the
    /// command after it in the same pipeline reads nothing rather than the same bytes twice.
    /// There is no terminal behind this: a command with neither reads nothing at all.
    fn standard_input(&mut self, redirections: &Redirections) -> Option<String> {
        // The text a here-document or here-string carries IS the input, and it is the
        // command's own: nothing of the tree is read for it.
        if let Some(text) = redirections.input_text.clone() {
            return Some(text);
        }
        if let Some(path) = redirections.input.clone() {
            return self.read_path(&path);
        }
        self.piped_input.take()
    }

    /// [`Self::standard_input`] for the utilities that READ it, with the status to answer when
    /// there is nothing to read: the utility's own failure for a file standard input was
    /// redirected from that the tree holds nothing at — reported on standard error in the
    /// utility's own wording, as a named file is — and a command that could not run when there
    /// was no input at all, since there is no terminal behind it here.
    fn read_standard_input(
        &mut self,
        name: &str,
        redirections: &Redirections,
    ) -> Result<String, i32> {
        let redirected = redirections.input.clone();
        match self.standard_input(redirections) {
            Some(input) => Ok(input),
            None => match redirected {
                Some(path) => {
                    let _ = self.write_stderr(
                        &format!("{name}: {path}: No such file or directory\n"),
                        redirections,
                    );
                    Err(self.config.statuses.failure)
                }
                None => Err(self.config.statuses.could_not_run),
            },
        }
    }

    /// Write `text` at `path` in the tree this session writes, replacing what is there or
    /// ADDING to it.
    ///
    /// `false` is a write that did not happen — a path the tree does not accept, an entry it
    /// will not let a program write, or a write past one of its bounds — which the command
    /// performing it reports.
    fn write_path(&mut self, path: &str, text: &str, append: bool) -> bool {
        if let Some(volume) = self.volume.as_mut() {
            return volume.write(path, text, append);
        }
        let Some(path) = file_path(path) else {
            return false;
        };
        let mut whole = if append {
            self.files.get(&path).cloned().unwrap_or_default()
        } else {
            String::new()
        };
        whole.push_str(text);
        if whole.len() > self.config.bounds.file_bytes {
            return false;
        }
        let displaced = self
            .files
            .get(&path)
            .map(|held| path.len().saturating_add(held.len()));
        if displaced.is_none() && self.files.len() >= self.config.bounds.files {
            return false;
        }
        let would_hold = self
            .tree_bytes
            .saturating_sub(displaced.unwrap_or(0))
            .saturating_add(path.len())
            .saturating_add(whole.len());
        if would_hold > self.config.bounds.tree_bytes {
            return false;
        }
        self.tree_bytes = would_hold;
        self.files.insert(path, whole);
        true
    }

    /// Whether the tree this session works over holds anything at `path` — a file, a
    /// directory, or (in a machine's tree) a device.
    fn tree_holds(&self, path: &str) -> bool {
        match &self.volume {
            Some(volume) => volume.exists(path),
            None => self.tree_holds_file(path) || self.tree_holds_directory(path),
        }
    }

    /// Whether `path` is a FILE of the tree this session works over.
    fn tree_holds_file(&self, path: &str) -> bool {
        match &self.volume {
            Some(volume) => volume.is_file(path),
            None => file_path(path).is_some_and(|path| self.files.contains_key(&path)),
        }
    }

    /// Whether `path` is a DIRECTORY of the tree this session works over.
    ///
    /// A session's own tree keys a file by its whole path and holds no directory entry, so a
    /// directory of it is a path some file lies under — which is also as long as it exists
    /// for: removing the last file under it leaves no directory behind.
    fn tree_holds_directory(&self, path: &str) -> bool {
        match &self.volume {
            Some(volume) => volume.is_directory(path),
            None => file_path(path).is_some_and(|path| {
                let under = format!("{path}/");
                !self.files.contains_key(&path)
                    && self.files.keys().any(|held| held.starts_with(&under))
            }),
        }
    }

    /// Make `path` a directory of the tree this session works over, with every directory
    /// above it, answering whether the tree now has it.
    ///
    /// In a session's own tree there is nothing to make: a path is a key, so a file may be
    /// written under `path` whether or not anything made it first, and the only answer that
    /// is not success is a FILE already holding that path.
    fn tree_make_directories(&mut self, path: &str) -> bool {
        match self.volume.as_mut() {
            Some(volume) => volume.make_directories(path),
            None => !self.tree_holds_file(path),
        }
    }

    /// Remove the file at `path` from the tree this session works over, answering whether
    /// there was one to remove.
    fn tree_remove_file(&mut self, path: &str) -> bool {
        if let Some(volume) = self.volume.as_mut() {
            return volume.remove_file(path);
        }
        let Some(path) = file_path(path) else {
            return false;
        };
        match self.files.remove(&path) {
            Some(contents) => {
                self.tree_bytes = self
                    .tree_bytes
                    .saturating_sub(contents.len().saturating_add(path.len()));
                true
            }
            None => false,
        }
    }

    /// The names directly under the DIRECTORY `directory` of the tree this session works
    /// over (its root for the empty path), each once and in path order.
    ///
    /// The tree keys a file by its whole path and holds no entry of its own for a directory,
    /// so a name here is either a file directly under that path or the first segment of a
    /// deeper file's path, which is what makes it a directory.
    fn tree_entry_names(&self, directory: &str) -> Vec<String> {
        let held = directory.trim();
        let held = held.strip_prefix("./").unwrap_or(held);
        let prefix = match held {
            "" | "." | "/" => String::new(),
            named => format!("{}/", named.trim_end_matches('/')),
        };
        let mut names: Vec<String> = Vec::new();
        for (path, _) in self.tree_files() {
            let Some(relative) = path.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let relative = relative.strip_prefix('/').unwrap_or(relative);
            let name = relative.split('/').next().unwrap_or(relative).to_string();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        names.sort();
        names
    }

    /// Perform the program the ENVIRONMENT this session runs in provides for `name`, when it
    /// provides one ([`EnvironmentPrograms`]).
    ///
    /// What that program answered is what the command answers, and each line it wrote is
    /// written the way a command writes one — so a redirection takes it, a command
    /// substitution captures it, and nothing else keeps it. A name the environment provides
    /// no program for is a command this session could not run, which is the same closed
    /// failure as a command neither catalog names.
    ///
    /// The program is handed this session's own FILES ([`ProgramFiles`]), so a program whose
    /// work is to carry a file somewhere reads the one the commands around it wrote and
    /// leaves the one they read back. The programs are taken OFF the session while it runs
    /// and put back after, which is what lets it be handed the session's tree at the same
    /// time; nothing else of the session is reachable from it.
    fn run_environment_program(
        &mut self,
        name: &str,
        arguments: &[String],
        redirections: &Redirections,
    ) -> i32 {
        self.performed_environment_program(name, arguments, redirections)
            .unwrap_or(self.config.statuses.could_not_run)
    }

    /// [`Self::run_environment_program`], answering `None` when this environment provides no
    /// program of that name at all — which is what lets a caller fall back to a utility this
    /// session performs itself, rather than shadowing the environment's own program with it.
    fn performed_environment_program(
        &mut self,
        name: &str,
        arguments: &[String],
        redirections: &Redirections,
    ) -> Option<i32> {
        let programs = self.programs.take()?;
        // What the command would READ on standard input, left in place while the program is
        // asked: a name this environment provides no program for falls back to a utility of
        // this session, which reads that same input.
        let input = self.peeked_standard_input(redirections);
        let answered = {
            let mut files = SessionFiles { session: self };
            programs.run_with_input(name, arguments, input.as_deref(), &mut files)
        };
        self.programs = Some(programs);
        let outcome = answered?;
        // The program performed the invocation, so the stream it was handed is consumed: a
        // stream is read by whoever reads it, and nothing after this command reads it twice.
        let _ = self.standard_input(redirections);
        for line in outcome.output {
            if !self.write_stdout(&format!("{line}\n"), redirections) {
                return Some(self.config.statuses.failure);
            }
        }
        Some(outcome.status)
    }

    /// What the command being performed would read from STANDARD INPUT, WITHOUT consuming it
    /// ([`Self::standard_input`] is the reading that does).
    fn peeked_standard_input(&self, redirections: &Redirections) -> Option<String> {
        if let Some(text) = redirections.input_text.clone() {
            return Some(text);
        }
        match redirections.input.as_deref() {
            Some(path) => self.read_path(path),
            None => self.piped_input.clone(),
        }
    }

    /// One assignment of a simple command as the name and value it sets, or `None` when it
    /// is a form this session does not perform — an array assignment, an appending
    /// assignment, or a value whose expansion it cannot resolve.
    fn assignment_of(
        &mut self,
        assignment: &::brush_parser::ast::Assignment,
    ) -> Option<(String, String)> {
        if assignment.append {
            return None;
        }
        let ::brush_parser::ast::AssignmentName::VariableName(name) = &assignment.name else {
            return None;
        };
        let ::brush_parser::ast::AssignmentValue::Scalar(value) = &assignment.value else {
            return None;
        };
        Some((name.clone(), self.expand_word(&value.value)?))
    }

    /// Set `name` to `value` in this session, unless it is already holding
    /// [`Bounds::session_bytes`] or [`Bounds::session_variables`] — `false` is then a
    /// command this session did not perform, which is what its caller reports.
    ///
    /// A change made while a command substitution is open is recorded on that substitution's
    /// undo log first ([`OpenSubstitution`]), so it can be rolled back when the subshell
    /// ends.
    fn set_variable(&mut self, name: String, value: String) -> bool {
        let displaced = self
            .variables
            .get(&name)
            .map(|held| name.len().saturating_add(held.len()));
        if displaced.is_none() && self.variables.len() >= self.config.bounds.session_variables {
            return false;
        }
        let would_hold = self
            .held_bytes
            .saturating_sub(displaced.unwrap_or(0))
            .saturating_add(name.len())
            .saturating_add(value.len());
        if would_hold > self.config.bounds.session_bytes {
            return false;
        }
        self.record_for_rollback(&name);
        self.held_bytes = would_hold;
        self.variables.insert(name, value);
        true
    }

    /// Remove `name` from this session, with the bytes it held.
    fn unset_variable(&mut self, name: &str) {
        self.record_for_rollback(name);
        if let Some(value) = self.variables.remove(name) {
            self.held_bytes = self
                .held_bytes
                .saturating_sub(name.len().saturating_add(value.len()));
        }
    }

    /// Record what the innermost open command substitution has to put `name` back to, when
    /// one is open and has not recorded it already — the FIRST touch is the one that names
    /// the value to go back to.
    fn record_for_rollback(&mut self, name: &str) {
        let held = self.variables.get(name).cloned();
        if let Some(undo) = self.substitution_undo.last_mut() {
            undo.entry(name.to_string()).or_insert(held);
        }
    }

    /// Write `text` to the program's standard OUTPUT, as one write of a command: to the
    /// path a redirection names, to the command substitution being captured, or — when
    /// neither — to what the session keeps for its caller.
    ///
    /// `false` is a write that did not happen, which is the failure the command performing
    /// it reports: a redirection the tree refused, or a capture past
    /// [`Bounds::substitution_bytes`].
    fn write_stdout(&mut self, text: &str, redirections: &Redirections) -> bool {
        if redirections.out_to_error {
            // `1>&2`: this command's output goes where its diagnostics go.
            return self.write_error_stream(text, redirections);
        }
        self.write_stream(redirections.out.clone(), text)
    }

    /// Write `text` to the program's standard ERROR, which is where a command reports what
    /// it could not do.
    ///
    /// With a redirection of its own it goes to the file or device that names, and NOT to
    /// where standard output is going: standard error is not what a command substitution
    /// captures, and not what a redirection of standard output takes. With none, it reaches
    /// what the session keeps only for a session given a MACHINE's tree, whose console is
    /// where a guest's diagnostics are read; for every other session a diagnostic is not part
    /// of what the program wrote, so nothing keeps it.
    fn write_stderr(&mut self, text: &str, redirections: &Redirections) -> bool {
        if redirections.error_to_out {
            // `2>&1`: this command's diagnostics go where its output goes, so they are what
            // a redirection takes, what a substitution captures and what the caller reads as
            // standard output.
            return self.write_stream(redirections.out.clone(), text);
        }
        self.write_error_stream(text, redirections)
    }

    /// [`Self::write_stderr`]'s write to the ERROR stream itself, with no duplication
    /// applied: the file or device a redirection of standard error names, and otherwise what
    /// the session keeps.
    fn write_error_stream(&mut self, text: &str, redirections: &Redirections) -> bool {
        if let Some((path, append)) = redirections.error.clone() {
            if !self.write_path(&path, text, append) {
                self.redirect_failed = true;
                return false;
            }
            return true;
        }
        // Kept for the caller that reports the two streams separately ([`Self::errors`]),
        // and ALSO on the console of a session given a machine's tree, which is where a
        // guest's diagnostics are read.
        self.keep_error(text);
        if self.volume.is_some() {
            self.keep_output(text);
        }
        true
    }

    /// [`Self::write_stdout`]'s and [`Self::write_stderr`]'s one write.
    fn write_stream(&mut self, target: Option<(String, bool)>, text: &str) -> bool {
        if let Some((path, append)) = target {
            if !self.write_path(&path, text, append) {
                self.redirect_failed = true;
                return false;
            }
            return true;
        }
        if let Some(capture) = self.captures.last_mut() {
            if capture.text.len().saturating_add(text.len()) > capture.limit {
                capture.overflowed = true;
                return false;
            }
            capture.text.push_str(text);
            return true;
        }
        self.keep_output(text);
        true
    }

    /// Keep `text` for the caller, one entry per LINE of it: a console and a log are read
    /// by the line, so a command that wrote several lines is kept as several.
    ///
    /// Nothing is kept for a session whose configuration asks for no output. A line past
    /// [`OutputLimits::line_bytes`] is truncated on a character boundary, and a line past
    /// [`OutputLimits::lines`] is not kept at all.
    fn keep_output(&mut self, text: &str) {
        let Some(limits) = self.config.output else {
            return;
        };
        if text.is_empty() {
            // Nothing was written, so there is no line to keep: a command that wrote no
            // bytes at all is not one that wrote an empty line.
            return;
        }
        for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
            if self.output.len() >= limits.lines {
                return;
            }
            let mut line = line.to_string();
            if line.len() > limits.line_bytes {
                let mut cut = limits.line_bytes;
                while cut > 0 && !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                line.truncate(cut);
            }
            self.output.push(line);
        }
    }

    /// [`Self::keep_output`] for standard ERROR, into the lines [`Self::errors`] answers
    /// with and under the same limits.
    fn keep_error(&mut self, text: &str) {
        let Some(limits) = self.config.output else {
            return;
        };
        if text.is_empty() {
            return;
        }
        for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
            if self.errors.len() >= limits.lines {
                return;
            }
            let mut line = line.to_string();
            if line.len() > limits.line_bytes {
                let mut cut = limits.line_bytes;
                while cut > 0 && !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                line.truncate(cut);
            }
            self.errors.push(line);
        }
    }

    /// Perform the command `name` with `arguments`, when it is one this session provides —
    /// [`BUILT_IN_COMMANDS`], and [`VOLUME_COMMANDS`] too for a session given a MACHINE's
    /// tree.
    ///
    /// `echo` writes a line of the program's output, without its newline when it is given
    /// `-n`. `cat` writes each file the tree holds for the paths it was given. `true` and
    /// `:` answer success and `false` answers failure, which is the whole of what those
    /// utilities do. `exit` ends the session with the status its argument names, defaulting
    /// to success, and a status outside 0..=255 is taken modulo 256 the way a shell takes an
    /// exit status. `export` sets the variables its `NAME=VALUE` arguments name and `unset`
    /// removes them. The machine commands are [`Self::run_volume_command`]'s, and a command
    /// neither catalog names is the ENVIRONMENT's to provide
    /// ([`Self::run_environment_program`]).
    fn run_built_in(
        &mut self,
        name: &str,
        arguments: &[String],
        redirections: &Redirections,
    ) -> i32 {
        // The catalogs are what this session provides: a command outside them needs a
        // program, which is the environment's to provide and not the shell's.
        if !BUILT_IN_COMMANDS.contains(&name) {
            // The text filters answer out of ONE input — a file, or what the stage before them
            // wrote — so they need no tree of the environment's and every session performs
            // them. The ENVIRONMENT's own program for that name comes FIRST: a caller whose
            // environment provides `wc` has its own catalog for it, and this must not shadow
            // it; the filter is what a session whose environment provides none falls back to.
            if TEXT_FILTER_COMMANDS.contains(&name) {
                if let Some(status) =
                    self.performed_environment_program(name, arguments, redirections)
                {
                    return status;
                }
                return self.run_text_filter(name, arguments, redirections);
            }
            if (self.volume.is_some() || self.working_tree) && VOLUME_COMMANDS.contains(&name) {
                // `ls` NAMES what the tree holds rather than changing it, and an environment
                // whose storage has its own catalog provides its own program for that name —
                // which this must not shadow, the same precedence the text filters above
                // take. The file utility is what a session whose environment provides none
                // falls back to.
                if name == LIST_COMMAND {
                    if let Some(status) =
                        self.performed_environment_program(name, arguments, redirections)
                    {
                        return status;
                    }
                }
                return self.run_volume_command(name, arguments, redirections);
            }
            return self.run_environment_program(name, arguments, redirections);
        }
        match name {
            "echo" => {
                let (words, newline) = match arguments.split_first() {
                    Some((first, rest)) if first == "-n" => (rest, false),
                    _ => (arguments, true),
                };
                let text = if newline {
                    format!("{}\n", words.join(" "))
                } else {
                    words.join(" ")
                };
                if self.write_stdout(&text, redirections) {
                    self.config.statuses.success
                } else {
                    self.config.statuses.failure
                }
            }
            "cat" => self.run_files_command(name, arguments, redirections),
            "true" | ":" => self.config.statuses.success,
            "false" => self.config.statuses.failure,
            "exit" => {
                let status = match arguments.first() {
                    Some(argument) => match argument.parse::<i64>() {
                        Ok(status) => {
                            i32::try_from(status.rem_euclid(256)).unwrap_or(self.config.statuses.failure)
                        }
                        // A status a shell cannot read is its own usage failure, and the
                        // session still ends.
                        Err(_) => self.config.statuses.failure,
                    },
                    None => self.config.statuses.success,
                };
                self.exit_status = Some(status);
                status
            }
            "export" => {
                for argument in arguments {
                    // `export NAME=VALUE` sets it; `export NAME` marks a variable this
                    // session already holds, and marking one it does not hold sets nothing.
                    if let Some((name, value)) = argument.split_once('=') {
                        if !self.set_variable(name.to_string(), value.to_string()) {
                            return self.config.statuses.could_not_run;
                        }
                    }
                }
                self.config.statuses.success
            }
            "unset" => {
                for argument in arguments {
                    self.unset_variable(argument);
                }
                self.config.statuses.success
            }
            _ => self.config.statuses.could_not_run,
        }
    }

    /// Perform one of the commands a session working over a TREE provides
    /// ([`VOLUME_COMMANDS`]) — a machine's ([`Self::in_volume`]) or its own working one
    /// ([`Self::with_file_utilities`]) — over that tree and nothing else.
    ///
    /// `printf` writes its format with its arguments substituted. `mkdir` makes a
    /// directory (`-p` makes the ones above it too), `touch` makes an empty file where the
    /// tree holds none, and `rm` removes a file (`-f` passes over one that is not there).
    /// `ls` writes the names the tree holds at the paths it was given ([`Self::run_list`]).
    /// `tee` writes what it READS into each file it names and passes it on
    /// ([`Self::run_tee`]).
    /// `test`, and its `[` spelling, answers a question about the tree or about two values.
    /// `sh -c` performs a program in THIS session, which is how a machine's init system
    /// runs one line of a script, `sh FILE` performs the script the tree holds at that path
    /// in the same way, `.` performs the commands of a file the tree holds in it
    /// ([`Self::run_dot`]), and `sleep` states the TIME the program took
    /// ([`Self::slept`]).
    fn run_volume_command(
        &mut self,
        name: &str,
        arguments: &[String],
        redirections: &Redirections,
    ) -> i32 {
        match name {
            // The utilities that work over the session's FILES are the shared reading of them
            // ([`super::posix_shell_files`]), performed over this session's own tree.
            name if posix_shell_files::FILE_COMMANDS.contains(&name) => {
                self.run_files_command(name, arguments, redirections)
            }
            "printf" => self.run_printf(arguments, redirections),
            TEE_COMMAND => self.run_tee(arguments, redirections),
            "test" | "[" => self.run_test(name, arguments),
            "sh" | "bash" => self.run_nested_program(arguments, redirections),
            "." | "source" => self.run_dot(arguments, redirections),
            "sleep" => self.run_sleep(arguments, redirections),
            _ => self.config.statuses.could_not_run,
        }
    }

    /// Perform one of the FILE utilities ([`posix_shell_files::FILE_COMMANDS`]) over this
    /// session's tree, and write what it produced through this session's own streams.
    ///
    /// WHAT each utility does and SAYS is the shared reading of it; what the session adds is
    /// where its files are ([`SessionFiles`]) and where its two streams go. `cat` with no
    /// operand reads STANDARD INPUT, which only the session can resolve — the file one was
    /// redirected from, or what the stage before it in a pipeline wrote.
    fn run_files_command(
        &mut self,
        name: &str,
        arguments: &[String],
        redirections: &Redirections,
    ) -> i32 {
        let reads_input = name == "cat" && !posix_shell_files::names_an_operand(arguments);
        let input = if reads_input {
            match self.read_standard_input(name, redirections) {
                Ok(input) => Some(input),
                Err(status) => return status,
            }
        } else {
            None
        };
        let performed = {
            let mut files = SessionFiles { session: self };
            posix_shell_files::perform_file_command(
                name,
                arguments,
                &mut files,
                input.as_deref(),
            )
        };
        let Some(outcome) = performed else {
            return self.config.statuses.could_not_run;
        };
        if !outcome.errors.is_empty() {
            let _ = self.write_stderr(&outcome.errors, redirections);
        }
        if !outcome.output.is_empty() && !self.write_stdout(&outcome.output, redirections) {
            return self.config.statuses.failure;
        }
        if outcome.succeeded {
            self.config.statuses.success
        } else {
            self.config.statuses.failure
        }
    }

    /// Perform one of the TEXT FILTERS ([`TEXT_FILTER_COMMANDS`]) over its one input: the file
    /// it names, or standard input.
    ///
    /// What it writes is what [`super::posix_shell_text_filters`] computed over those bytes —
    /// a digest, a count, a selection — and nothing of the invocation is interpreted here. An
    /// option or operand form that module does not perform is a command this session could not
    /// run, a file the tree holds none of is reported the way the file utilities report one,
    /// and a filter with no input at all could not run either: there is no terminal behind
    /// standard input here.
    fn run_text_filter(
        &mut self,
        name: &str,
        arguments: &[String],
        redirections: &Redirections,
    ) -> i32 {
        let Some(filter) = Filter::read(name, arguments) else {
            return self.config.statuses.could_not_run;
        };
        let (label, input) = match filter.file() {
            Some(path) => match self.read_path(path) {
                Some(held) => (path.to_string(), held),
                None => {
                    let _ = self.write_stderr(
                        &format!("{name}: {path}: No such file or directory\n"),
                        redirections,
                    );
                    return self.config.statuses.failure;
                }
            },
            None => match self.read_standard_input(name, redirections) {
                Ok(input) => (STANDARD_INPUT_LABEL.to_string(), input),
                Err(status) => return status,
            },
        };
        let written = match filter.perform(&label, &input) {
            Ok(performed) => performed,
            // An input the utility cannot compute over: what it writes about it goes to
            // standard error, and nothing is written to standard output.
            Err(refusal) => {
                let _ = self.write_stderr(&format!("{refusal}\n"), redirections);
                return self.config.statuses.failure;
            }
        };
        if !self.write_stdout(&written.written, redirections) {
            return self.config.statuses.failure;
        }
        // The status of a utility that reports a SELECTION is that selection
        // ([`super::posix_shell_text_filters::Selection`]), which is what an `if` around such
        // a command branches on; every other one answers success for an input it computed
        // over.
        if written.selected == Some(false) {
            return self.config.statuses.failure;
        }
        self.config.statuses.success
    }

    /// `sleep`: the time the program asked to take, added to what this session has slept
    /// ([`Self::slept`]).
    ///
    /// No thread waits for it. This session performs a program's commands in process and
    /// has no clock of its own to spend, so what a `sleep` states is the TIME THE PROGRAM
    /// TOOK: the caller whose API reports a duration, or converges a submitted task on one,
    /// honours it, and a caller that reports no duration performs the sleep in no time at
    /// all. What one program may ask for in total is bounded ([`Bounds::sleep_seconds`]).
    ///
    /// "The sleep utility shall suspend execution for at least the integral number of
    /// seconds specified by the time operand": a time this command cannot read is the
    /// utility's own usage failure, and so is a missing operand.
    fn run_sleep(&mut self, arguments: &[String], redirections: &Redirections) -> i32 {
        let Some(operand) = arguments.iter().find(|argument| !argument.starts_with('-')) else {
            let _ = self.write_stderr("sleep: missing operand\n", redirections);
            return self.config.statuses.failure;
        };
        // The suffixes the common implementation accepts beside plain seconds.
        let (value, unit) = match operand.chars().next_back() {
            Some(suffix @ ('s' | 'm' | 'h' | 'd')) => (
                &operand[..operand.len().saturating_sub(suffix.len_utf8())],
                suffix,
            ),
            _ => (operand.as_str(), 's'),
        };
        let Ok(seconds) = value.parse::<f64>() else {
            let _ = self.write_stderr(
                &format!("sleep: invalid time interval '{operand}'\n"),
                redirections,
            );
            return self.config.statuses.failure;
        };
        if !seconds.is_finite() || seconds < 0.0 {
            let _ = self.write_stderr(
                &format!("sleep: invalid time interval '{operand}'\n"),
                redirections,
            );
            return self.config.statuses.failure;
        }
        let factor = match unit {
            'm' => 60.0,
            'h' => 3_600.0,
            'd' => 86_400.0,
            _ => 1.0,
        };
        let asked = ::std::time::Duration::try_from_secs_f64(seconds * factor)
            .unwrap_or(::std::time::Duration::ZERO);
        let ceiling = ::std::time::Duration::from_secs(self.config.bounds.sleep_seconds);
        self.slept = self.slept.saturating_add(asked).min(ceiling);
        self.config.statuses.success
    }

    /// `tee`: "copy standard input to each OUTPUT FILE and also to standard output".
    ///
    /// What it reads is its standard input ([`Self::read_standard_input`]) — a here-document,
    /// a file an input redirection named, or what the stage before it in a pipeline wrote —
    /// and it is written into every file operand, from the start or APPENDED for `-a`
    /// ("append the output to the files"), and then passed on. A write the tree will not take
    /// is reported in the utility's own wording and the command answers failure, without
    /// stopping the files after it.
    fn run_tee(&mut self, arguments: &[String], redirections: &Redirections) -> i32 {
        let append = arguments
            .iter()
            .any(|argument| argument == "-a" || argument == "--append");
        let paths: Vec<String> = arguments
            .iter()
            .filter(|argument| !argument.starts_with('-'))
            .cloned()
            .collect();
        let input = match self.read_standard_input(TEE_COMMAND, redirections) {
            Ok(input) => input,
            Err(status) => return status,
        };
        let mut status = self.config.statuses.success;
        for path in paths {
            if !self.write_path(&path, &input, append) {
                let _ = self.write_stderr(
                    &format!("{TEE_COMMAND}: {path}: Cannot write to file\n"),
                    redirections,
                );
                status = self.config.statuses.failure;
            }
        }
        if !self.write_stdout(&input, redirections) {
            status = self.config.statuses.failure;
        }
        status
    }

    /// `printf`: the format with its arguments substituted, on standard output.
    ///
    /// The conversions performed are the ones a boot script writes with — `%s`, `%d`/`%i`
    /// and `%%` — and the escapes are `\n`, `\t`, `\r` and `\\`. The format is applied
    /// ONCE, however many arguments are left over, and a conversion outside that set is one
    /// this command does not perform: nothing is written and it answers failure.
    fn run_printf(&mut self, arguments: &[String], redirections: &Redirections) -> i32 {
        let Some((format, rest)) = arguments.split_first() else {
            let _ = self.write_stderr("printf: usage: printf format [arguments]\n", redirections);
            return self.config.statuses.failure;
        };
        let Some(text) = formatted(format, rest) else {
            let _ = self.write_stderr(
                &format!("printf: {format}: invalid format\n"),
                redirections,
            );
            return self.config.statuses.failure;
        };
        if self.write_stdout(&text, redirections) {
            self.config.statuses.success
        } else {
            self.config.statuses.failure
        }
    }

    /// `test` / `[`: the question its arguments ask, answered as a status — success for
    /// true, failure for false.
    ///
    /// The questions answered are the ones a boot script asks: whether the tree holds a
    /// path (`-e`), a file (`-f`), a directory (`-d`) or a file with something in it
    /// (`-s`), whether a string is empty (`-z`) or not (`-n`), whether two strings are
    /// equal (`=`, `!=`), and how two integers compare (`-eq`, `-ne`, `-lt`, `-le`, `-gt`,
    /// `-ge`). A single argument is true when it is not empty. A form outside that set is
    /// one this command does not answer, which is reported as a command that could not run
    /// rather than as a false answer.
    fn run_test(&mut self, name: &str, arguments: &[String]) -> i32 {
        let mut arguments = arguments.to_vec();
        if name == "[" {
            // The `[` spelling is the same utility, and its last argument is the closing
            // bracket rather than part of the question.
            if arguments.last().map(String::as_str) != Some("]") {
                return self.config.statuses.could_not_run;
            }
            arguments.pop();
        }
        let negated = arguments.first().map(String::as_str) == Some("!");
        if negated {
            arguments.remove(0);
        }
        let Some(answer) = self.test_answer(&arguments) else {
            return self.config.statuses.could_not_run;
        };
        if answer != negated {
            self.config.statuses.success
        } else {
            self.config.statuses.failure
        }
    }

    /// [`Self::run_test`]'s reading of the question, or `None` for a form it does not
    /// answer.
    ///
    /// The questions about a FILE are the shared reading of them
    /// ([`posix_shell_files::file_test_answer`]) over this session's own tree, and the
    /// questions about VALUES are the same wherever the utility is performed
    /// ([`value_test_answer`]).
    fn test_answer(&mut self, arguments: &[String]) -> Option<bool> {
        let answered = {
            let files = SessionFiles { session: self };
            posix_shell_files::file_test_answer(arguments, &files)
        };
        answered.or_else(|| value_test_answer(arguments))
    }

    /// `sh -c PROGRAM` (and `bash -c`), and `sh FILE`: the program performed in THIS
    /// session, under the same bounds and against the same tree and variables.
    ///
    /// `sh FILE` is the other way a program of an environment with storage is written — a
    /// script the commands before it left in the tree — so the text performed is read out of
    /// the SAME tree ([`Self::read_path`]) and nothing else: no host path is opened for it,
    /// and a path the tree holds no file for is reported the way a shell reports a script it
    /// cannot open, with the status it exits with.
    ///
    /// It counts against the same budget and the same DEPTH a command substitution does
    /// ([`Bounds::substitutions`], [`Bounds::substitution_depth`]): a nested shell is the
    /// other way one program can run another, so what bounds this module's own recursion has
    /// to count both.
    ///
    /// Any other invocation — an interactive shell, a login shell, a program read from
    /// standard input — needs something this session does not have, so it is a command that
    /// could not run.
    fn run_nested_program(&mut self, arguments: &[String], redirections: &Redirections) -> i32 {
        let Some((first, rest)) = arguments.split_first() else {
            return self.config.statuses.could_not_run;
        };
        let program = if first == "-c" {
            match rest.first() {
                Some(program) => program.clone(),
                None => return self.config.statuses.could_not_run,
            }
        } else if first.starts_with('-') {
            return self.config.statuses.could_not_run;
        } else {
            match self.read_path(first) {
                Some(text) => text,
                None => {
                    let _ = self.write_stderr(
                        &format!("sh: {first}: No such file or directory\n"),
                        redirections,
                    );
                    return self.config.statuses.could_not_run;
                }
            }
        };
        if self.substitutions_left == 0 || self.nesting_depth() >= self.config.bounds.substitution_depth
        {
            return self.config.statuses.could_not_run;
        }
        self.substitutions_left = self.substitutions_left.saturating_sub(1);
        self.nested_programs = self.nested_programs.saturating_add(1);
        let status = self.run_program(&program);
        self.nested_programs = self.nested_programs.saturating_sub(1);
        status
    }

    /// `. FILE` (and its bash `source` spelling): the commands of a file the tree holds,
    /// performed in THIS session.
    ///
    /// The dot utility is specified to "execute commands from the file in the current
    /// environment", so the variables the file's own assignments set are the session's once
    /// it has run — which is how a program reads a file of assignments the environment
    /// published to it, a distribution's release file or a tool's profile script. The file
    /// is read out of the tree this session works over and out of nothing else: there is no
    /// search path here, and no host path, so a path the tree holds no file for is one this
    /// utility could not read — reported on standard error, with the status the utility
    /// answers for a file it could not read rather than with the file's own.
    ///
    /// It runs another program in this session, so it spends the same budget and counts
    /// against the same DEPTH an `sh -c` program does ([`Self::run_nested_program`]): a file
    /// that sources itself is bounded by them.
    fn run_dot(&mut self, arguments: &[String], redirections: &Redirections) -> i32 {
        let Some(path) = arguments.first() else {
            let _ = self.write_stderr(".: filename argument required\n", redirections);
            return self.config.statuses.failure;
        };
        let Some(contents) = self.read_path(path) else {
            let _ = self.write_stderr(
                &format!(".: {path}: No such file or directory\n"),
                redirections,
            );
            return self.config.statuses.failure;
        };
        if self.substitutions_left == 0
            || self.nesting_depth() >= self.config.bounds.substitution_depth
        {
            return self.config.statuses.could_not_run;
        }
        self.substitutions_left = self.substitutions_left.saturating_sub(1);
        self.nested_programs = self.nested_programs.saturating_add(1);
        let status = self.run_program(&contents);
        self.nested_programs = self.nested_programs.saturating_sub(1);
        status
    }

    /// How deeply one program is running inside another right now: the substitutions being
    /// captured and the nested shells around them, which are the two ways a program here
    /// runs another.
    fn nesting_depth(&self) -> usize {
        self.captures.len().saturating_add(self.nested_programs)
    }

    /// `word` with its expansions resolved, or `None` when it carries an expansion this
    /// session cannot resolve.
    ///
    /// The pieces come from `brush-parser`'s word parser, so what is quoted, what is a
    /// parameter expansion and what is literal text is the shell's own reading of the word.
    /// A parameter this session holds nothing for expands to nothing, which is what an unset
    /// parameter expands to. A command substitution is performed by every session
    /// ([`Self::expand_substitution`]); an arithmetic expansion is resolved only by a session
    /// that runs in a volume ([`Self::expand_arithmetic`]), so for a session with no tree —
    /// and for a tilde prefix whichever session carries it, there is no home directory here —
    /// the word has no expansion and the command carrying it is not run. Nor has a word that
    /// expands past [`Bounds::expansion_bytes`], or one whose expansion would take the line
    /// past [`Bounds::line_expansion_bytes`].
    fn expand_word(&mut self, word: &str) -> Option<String> {
        let options = ::brush_parser::ParserOptions::default();
        let pieces = ::brush_parser::word::parse(word, &options).ok()?;
        let expanded = self.expand_pieces(&pieces, self.config.bounds.word_depth)?;
        self.expansion_left = self.expansion_left.checked_sub(expanded.len())?;
        Some(expanded)
    }

    /// [`Self::expand_word`]'s walk over the pieces a word parsed into, with `depth` levels
    /// of quoting left: a word nested past [`Bounds::word_depth`] has no expansion here, so
    /// this walk's own recursion stays in range whatever the word nests.
    fn expand_pieces(
        &mut self,
        pieces: &[::brush_parser::word::WordPieceWithSource],
        depth: usize,
    ) -> Option<String> {
        let depth = depth.checked_sub(1)?;
        let mut expanded = String::new();
        for piece in pieces {
            let addition = match &piece.piece {
                ::brush_parser::word::WordPiece::Text(text)
                | ::brush_parser::word::WordPiece::SingleQuotedText(text) => text.clone(),
                ::brush_parser::word::WordPiece::DoubleQuotedSequence(inner) => {
                    let inner = inner.clone();
                    self.expand_pieces(&inner, depth)?
                }
                ::brush_parser::word::WordPiece::ParameterExpansion(parameter) => {
                    self.expand_parameter(parameter)?
                }
                ::brush_parser::word::WordPiece::CommandSubstitution(program)
                | ::brush_parser::word::WordPiece::BackquotedCommandSubstitution(program) => {
                    let program = program.clone();
                    self.expand_substitution(&program)?
                }
                ::brush_parser::word::WordPiece::ArithmeticExpression(expression) => {
                    let text = expression.value.clone();
                    self.expand_arithmetic(&text)?
                }
                ::brush_parser::word::WordPiece::AnsiCQuotedText(_)
                | ::brush_parser::word::WordPiece::GettextDoubleQuotedSequence(_)
                | ::brush_parser::word::WordPiece::EscapeSequence(_)
                | ::brush_parser::word::WordPiece::TildeExpansion(_) => return None,
            };
            if expanded.len().saturating_add(addition.len()) > self.config.bounds.expansion_bytes {
                return None;
            }
            expanded.push_str(&addition);
        }
        Some(expanded)
    }

    /// The value of a command SUBSTITUTION: what the commands inside it wrote to standard
    /// output, "with one or more trailing newline characters removed", as the shell
    /// specifies.
    ///
    /// The commands run in THIS session — the same variables, the same tree, the same
    /// budget, the same catalogs and the same environment programs — so a substitution
    /// cannot reach anything its session cannot. It is a SUBSHELL: an `exit` inside one ends
    /// the substitution rather than the session, and the variables it set or removed are put
    /// back off the undo log it recorded ([`Self::record_for_rollback`]), which costs what it
    /// changed rather than a copy of the store.
    ///
    /// Its own cost comes out of the same budgets: performing one spends a command of
    /// [`Bounds::commands`] before anything else — so how many a line may open is bounded
    /// whatever the text they carry performs — and the commands inside it spend their own.
    ///
    /// `None` is a substitution with no value: one past [`Bounds::substitutions`],
    /// [`Bounds::substitution_depth`] or the command budget does not run, and one whose
    /// output went past [`Bounds::substitution_bytes`] has no value rather than a clipped
    /// one.
    fn expand_substitution(&mut self, program: &str) -> Option<String> {
        if self.substitutions_left == 0
            || self.commands_left == 0
            || self.nesting_depth() >= self.config.bounds.substitution_depth
        {
            return None;
        }
        self.substitutions_left = self.substitutions_left.saturating_sub(1);
        self.commands_left = self.commands_left.saturating_sub(1);
        self.captures.push(Capture {
            limit: self.config.bounds.substitution_bytes,
            ..Capture::default()
        });
        self.substitution_undo.push(BTreeMap::new());
        let held_exit = self.exit_status.take();
        let held_bytes = self.held_bytes;
        let status = self.run_program(program);
        self.exit_status = held_exit;
        for (name, held) in self.substitution_undo.pop().unwrap_or_default() {
            match held {
                Some(value) => self.variables.insert(name, value),
                None => self.variables.remove(&name),
            };
        }
        // The store is back to what it was, so what it holds is back to what it held.
        self.held_bytes = held_bytes;
        // The LAST substitution performed is the one a command with no command name answers
        // with, so this replaces what a substitution performed earlier in the same command
        // left.
        self.last_substitution_status = Some(status);
        let capture = self.captures.pop()?;
        if capture.overflowed {
            return None;
        }
        Some(capture.text.trim_end_matches('\n').to_string())
    }

    /// The value of an ARITHMETIC expansion (`$((…))`): the number its expression comes to
    /// in this session.
    ///
    /// The text's own parameter expansions are resolved first — `$((count + 1))` and
    /// `$(($count + 1))` are both written — and then the expression is parsed by
    /// `brush-parser`'s arithmetic grammar and evaluated over this session's variables
    /// ([`super::posix_shell_arithmetic`]).
    ///
    /// `None` is an expansion with no value: a session that was given no machine's tree does
    /// not perform the form, and neither does an expression past the bound a parse is given
    /// ([`within_parse_bounds`], applied to the text the expansion materialised rather than
    /// only to the line it came from), one that does not parse, one this module does not
    /// evaluate, or one whose arithmetic has no answer (a division by zero, an overflow).
    fn expand_arithmetic(&mut self, text: &str) -> Option<String> {
        if !self.works_over_a_tree() {
            return None;
        }
        let resolved = self.expand_word(text).unwrap_or_else(|| text.to_string());
        // The expansion above can have MATERIALISED text far longer and far more deeply
        // nested than the line itself — a variable's value expands inside the expression —
        // so the same bound a line is read under is applied to it before it reaches a
        // grammar: a parser costs a level of recursion per nested group, and that recursion
        // is what this keeps in range.
        if !within_parse_bounds(&resolved, &self.config.bounds) {
            return None;
        }
        let expression = ::brush_parser::arithmetic::parse(&resolved).ok()?;
        let depth = self.config.bounds.word_depth;
        let value = posix_shell_arithmetic::evaluate(&expression, depth, self)?;
        Some(value.to_string())
    }

    /// The value a parameter expansion resolves to in this session.
    ///
    /// The plain form (`$NAME`, `${NAME}`) is the variable's value, empty when the session
    /// holds none, and the default-value form (`${NAME:-WORD}`) is that value or the default
    /// the expansion names. Every other form — a substring, a replacement, a length, an
    /// indirection, a positional or special parameter — is one this session does not
    /// resolve.
    fn expand_parameter(&self, parameter: &::brush_parser::word::ParameterExpr) -> Option<String> {
        match parameter {
            ::brush_parser::word::ParameterExpr::Parameter {
                parameter: ::brush_parser::word::Parameter::Named(name),
                indirect: false,
            } => Some(self.variable(name).unwrap_or_default().to_string()),
            ::brush_parser::word::ParameterExpr::UseDefaultValues {
                parameter: ::brush_parser::word::Parameter::Named(name),
                indirect: false,
                default_value,
                ..
            } => {
                let held = self.variable(name).unwrap_or_default();
                if held.is_empty() {
                    Some(default_value.clone().unwrap_or_default())
                } else {
                    Some(held.to_string())
                }
            }
            _ => None,
        }
    }
}

/// The variables an arithmetic expansion of this session reads and assigns: the session's
/// own, under the session's own bounds — an assignment the session will not take (it is at
/// [`Bounds::session_bytes`] or [`Bounds::session_variables`]) is one the expansion could
/// not perform.
impl ArithmeticVariables for ShellSession {
    fn arithmetic_value(&self, name: &str) -> Option<String> {
        self.variable(name).map(str::to_string)
    }

    fn set_arithmetic_value(&mut self, name: &str, value: i64) -> bool {
        self.set_variable(name.to_string(), value.to_string())
    }
}

/// What the `test` utility (and its `[ ... ]` spelling) answers for the questions that are
/// about VALUES alone — the whole of the utility that needs nothing but its arguments — or
/// `None` for a form this does not answer, which includes every question about a file.
///
/// POSIX's `test` "evaluate[s] expression" and answers it as a status, and these are the
/// primaries whose answer is a property of the words themselves: an expression of no
/// arguments is false, one of a single argument is true when that argument is not the empty
/// string, `-z`/`-n` ask whether a string is empty or not, `=`/`!=` compare two strings, and
/// `-eq`/`-ne`/`-lt`/`-le`/`-gt`/`-ge` compare two integers (`==` is the equality spelling
/// bash also takes). A primary about a FILE (`-e`, `-f`, `-d`, `-s`, `-r`, `-w`) is a
/// question about a tree, so it is the caller's to answer where it holds one and unanswerable
/// — `None` — where it holds none.
///
/// Shared because the reading is the same wherever the utility is performed: a session with
/// a tree of its own answers the file primaries itself and falls back to this
/// ([`ShellSession::test_answer`]), and a session with no tree at all has only these.
pub fn value_test_answer(arguments: &[String]) -> Option<bool> {
    match arguments {
        [] => Some(false),
        [value] => Some(!value.is_empty()),
        [operator, value] => match operator.as_str() {
            "-z" => Some(value.is_empty()),
            "-n" => Some(!value.is_empty()),
            _ => None,
        },
        [left, operator, right] => match operator.as_str() {
            "=" | "==" => Some(left == right),
            "!=" => Some(left != right),
            "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                let left = left.trim().parse::<i64>().ok()?;
                let right = right.trim().parse::<i64>().ok()?;
                Some(match operator.as_str() {
                    "-eq" => left == right,
                    "-ne" => left != right,
                    "-lt" => left < right,
                    "-le" => left <= right,
                    "-gt" => left > right,
                    _ => left >= right,
                })
            }
            _ => None,
        },
        _ => None,
    }
}

/// `format` with `arguments` substituted, as `printf` writes it, or `None` for a format
/// this session does not perform.
///
/// The conversions are `%s` (the argument as it is), `%d` and `%i` (the argument read as an
/// integer, and a value that is not one is worth zero, as the utility reads it) and `%%`
/// (one per cent sign). The escapes are `\n`, `\t`, `\r` and `\\`. An argument the format
/// has no conversion for is left out, and a conversion with no argument left takes the
/// empty string, which is what the utility writes for one.
fn formatted(format: &str, arguments: &[String]) -> Option<String> {
    let mut written = String::new();
    let mut taken = arguments.iter();
    let mut characters = format.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => match characters.next() {
                Some('n') => written.push('\n'),
                Some('t') => written.push('\t'),
                Some('r') => written.push('\r'),
                Some('\\') => written.push('\\'),
                // Every other escape is one this command does not perform.
                _ => return None,
            },
            '%' => match characters.next() {
                Some('%') => written.push('%'),
                Some('s') => written.push_str(taken.next().map_or("", String::as_str)),
                Some('d' | 'i') => {
                    let argument = taken.next().map_or("", String::as_str);
                    let value = argument.trim().parse::<i64>().unwrap_or(0);
                    written.push_str(&value.to_string());
                }
                // Every other conversion is one this command does not perform.
                _ => return None,
            },
            plain => written.push(plain),
        }
    }
    Some(written)
}

#[cfg(test)]
mod tests {
    use super::{
        Bounds, EnvironmentPrograms, OutputLimits, ProgramFiles, ProgramOutcome, SessionConfig,
        ShellSession, ShellVolume, Statuses, BUILT_IN_COMMANDS,
    };
    use std::collections::BTreeMap;

    /// A session that keeps its output, with the default statuses and bounds.
    fn session() -> ShellSession {
        ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                output: Some(OutputLimits {
                    lines: 8,
                    line_bytes: 64,
                }),
                ..SessionConfig::default()
            },
        )
    }

    /// Perform every line of `program` in one session and answer it.
    fn run(program: &str) -> ShellSession {
        let mut held = session();
        for line in program.lines() {
            held.run(line);
            if held.exit_status().is_some() {
                break;
            }
        }
        held
    }

    /// RULE: a command invoked through a wrapper that only changes HOW it is run is that
    /// command — the wrapper's own words are the command line — and a wrapper naming no
    /// utility runs nothing.
    #[test]
    fn a_wrapper_runs_the_command_it_names() {
        let statuses = Statuses::default();
        // (case, the line, the status, the lines it wrote)
        let rows: &[(&str, &str, i32, &[&str])] = &[
            ("nohup around a utility", "nohup echo kept", statuses.success, &["kept"]),
            (
                "setsid around a utility",
                "setsid echo kept",
                statuses.success,
                &["kept"],
            ),
            (
                "one wrapper around another",
                "nohup setsid echo kept",
                statuses.success,
                &["kept"],
            ),
            (
                "a wrapper around a command this session cannot run",
                "nohup nothing-of-this-session",
                statuses.could_not_run,
                &[],
            ),
            ("a wrapper naming no utility", "nohup", statuses.could_not_run, &[]),
        ];
        for (case, line, status, written) in rows {
            let mut held = session();
            let answered = held.run(line);
            assert_eq!(
                answered, *status,
                "case {case:?}: expected status {status}, got {answered}"
            );
            assert_eq!(
                held.output(),
                *written,
                "case {case:?}: expected {written:?}, got {:?}",
                held.output()
            );
        }
    }

    /// RULE: only the catalog's utilities are performed; every other command and every form
    /// this session does not perform answers "could not run" and writes nothing.
    #[test]
    fn only_the_catalog_is_performed() {
        let statuses = Statuses::default();
        // (case, the line, the status it answers, the lines it wrote)
        let rows: [(&str, &str, i32, &[&str]); 9] = [
            ("echo", "echo one two", statuses.success, &["one two"]),
            ("true", "true", statuses.success, &[]),
            (":", ":", statuses.success, &[]),
            ("false", "false", statuses.failure, &[]),
            (
                "a host program",
                "curl https://example.invalid",
                statuses.could_not_run,
                &[],
            ),
            (
                "an output redirection writes the session's own file",
                "echo out > kept.txt",
                statuses.success,
                &[],
            ),
            (
                "an input redirection",
                "cat < kept.txt",
                statuses.could_not_run,
                &[],
            ),
            (
                "another host program, which the environment provides none of either",
                "whoami",
                statuses.could_not_run,
                &[],
            ),
            (
                "a compound command",
                "if true; then echo inside; fi",
                statuses.could_not_run,
                &[],
            ),
        ];
        for (case, line, expected_status, expected_output) in rows {
            let mut held = session();
            let status = held.run(line);
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected status {expected_status}, got {status}"
            );
            assert_eq!(
                held.output(),
                expected_output,
                "case {case:?}: expected {expected_output:?}, got {:?}",
                held.output()
            );
        }
        assert_eq!(
            BUILT_IN_COMMANDS.len(),
            8,
            "case the catalog: expected the eight session-local utilities, got {BUILT_IN_COMMANDS:?}"
        );
    }

    /// RULE: a pipeline connects its stages — what one stage writes to standard output is the
    /// standard input of the next — so only the last stage's output is the pipeline's, and a
    /// filter computes over the bytes the stage before it produced.
    ///
    /// The rows differ along what the rule generalises over: how many stages the pipeline has,
    /// which filter reads the stage before it, whether a stage redirects its own output away
    /// from the pipe, and whether a stage writes anything at all.
    #[test]
    fn a_pipeline_connects_what_one_stage_wrote_to_the_next() {
        let statuses = Statuses::default();
        // (case, the program, the status its last line answered, the lines it wrote)
        let rows: [(&str, &str, i32, &[&str]); 8] = [
            (
                "one stage reads what the stage before it wrote",
                "echo abc | cat",
                statuses.success,
                &["abc"],
            ),
            (
                "the stage before the last one writes nothing to the session",
                "echo abc | wc -c",
                statuses.success,
                // `echo` ends its line with a newline, so four bytes reach the filter.
                &["4"],
            ),
            (
                "three stages, each over what the one before it produced",
                "echo abcdef | wc -c | cut -c1",
                statuses.success,
                &["7"],
            ),
            (
                // The session this test drives keeps lines of at most 64 bytes, so the digest
                // is read back through the selection the recorded buildspec writes.
                "a digest over the bytes the stage before it wrote",
                "echo -n abc | sha256sum | cut -c1-64",
                statuses.success,
                &["ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"],
            ),
            (
                "a filter with nothing piped into it could not run",
                "wc -c",
                statuses.could_not_run,
                &[],
            ),
            (
                "a stage that redirects its own output passes an empty stream on",
                "echo abc > kept.txt | wc -c",
                statuses.success,
                &["0"],
            ),
            (
                "the pipeline's status is its last stage's",
                "echo abc | false",
                statuses.failure,
                &[],
            ),
            (
                "a stage this session has no utility for wrote nothing to pass on",
                "mvn package | wc -c",
                statuses.success,
                &["0"],
            ),
        ];
        for (case, program, expected_status, expected_output) in rows {
            let mut held = session();
            let mut status = statuses.success;
            for line in program.lines() {
                status = held.run(line);
            }
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected status {expected_status}, got {status}"
            );
            assert_eq!(
                held.output(),
                expected_output,
                "case {case:?}: expected {expected_output:?}, got {:?}",
                held.output()
            );
        }
    }

    /// RULE: a stage whose output is past what a pipe may carry ends the pipeline: the stage
    /// after it is not run over a stream nothing produced.
    #[test]
    fn a_stage_past_the_pipe_bound_ends_the_pipeline() {
        let statuses = Statuses::default();
        let mut held = ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                bounds: Bounds {
                    // Smaller than the bytes the first stage writes, which is what puts that
                    // stage past the bound.
                    substitution_bytes: 4,
                    ..Bounds::default()
                },
                output: Some(OutputLimits {
                    lines: 8,
                    line_bytes: 64,
                }),
                ..SessionConfig::default()
            },
        );
        let status = held.run("echo abcdefgh | wc -c");
        assert_eq!(
            status, statuses.failure,
            "case a stage past the pipe bound: expected the failure of the write that did not \
             happen, got {status}"
        );
        assert!(
            held.output().is_empty(),
            "case a stage past the pipe bound: the stage after it must not have run, got {:?}",
            held.output()
        );
    }

    /// RULE: a command substitution over a pipeline is worth what the LAST stage wrote, with    /// its trailing newlines removed — so a program's own computation is the word it
    /// substituted.
    #[test]
    fn a_substituted_pipeline_is_worth_its_last_stage() {
        // (case, the program, the value the variable ends holding)
        let rows: [(&str, &str, &str); 4] = [
            (
                "a digest of a variable's value",
                "SECRET=abc\nDIGEST=$(printf '%s' \"$SECRET\" | sha256sum | cut -c1-64)",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "the byte count of a variable's value",
                "SECRET=abcdef\nLENGTH=$(printf '%s' \"$SECRET\" | wc -c)",
                "6",
            ),
            (
                "a count over a value with multi-byte characters",
                "SECRET=h\u{e9}llo\nLENGTH=$(printf '%s' \"$SECRET\" | wc -c)",
                "6",
            ),
            (
                "a selection out of what the stage before it wrote",
                "SECRET=one:two\nFIELD=$(printf '%s' \"$SECRET\" | cut -d: -f2)",
                "two",
            ),
        ];
        for (case, program, expected) in rows {
            // The file utilities are what `printf` is one of, so the session is the one an
            // environment with storage of its own drives.
            let mut held = session().with_file_utilities();
            for line in program.lines() {
                held.run(line);
            }
            let assigned = program
                .lines()
                .next_back()
                .and_then(|line| line.split_once('='))
                .map(|(name, _)| name)
                .unwrap_or_default();
            let value = held.variable(assigned);
            assert_eq!(
                value,
                Some(expected),
                "case {case:?}: expected {assigned} to hold {expected:?}, got {value:?}"
            );
        }
    }

    /// RULE: a program's commands write FILES in the session's own tree — an output
    /// redirection keeps what the command wrote under the path it named (`>` from the start,
    /// `>>` added to what the file holds), `cat` reads one back, and a path the tree holds no
    /// file for is one `cat` could not read.
    ///
    /// The rows differ along what the rule generalises over: which redirection operator the
    /// program used, how many commands wrote the file, the path's spelling, whether the file
    /// was ever written, and which command the redirection applies to.
    #[test]
    fn a_program_s_commands_write_files_in_the_session_s_tree() {
        let statuses = Statuses::default();
        // (case, the program, the status its last line answered, the lines it wrote)
        let rows: [(&str, &str, i32, &[&str]); 8] = [
            (
                "a redirected write read back",
                "echo first > out.txt\ncat out.txt",
                statuses.success,
                &["first"],
            ),
            (
                "a second write from the start replaces the file",
                "echo first > out.txt\necho second > out.txt\ncat out.txt",
                statuses.success,
                &["second"],
            ),
            (
                "an appending write adds to it",
                "echo first > out.txt\necho second >> out.txt\ncat out.txt",
                statuses.success,
                &["first", "second"],
            ),
            (
                "an appending write to a file nothing wrote yet",
                "echo only >> out.txt\ncat out.txt",
                statuses.success,
                &["only"],
            ),
            (
                "the same file named with and without a leading ./",
                "echo first > ./out.txt\ncat out.txt",
                statuses.success,
                &["first"],
            ),
            (
                "a path nothing wrote",
                "cat missing.txt",
                statuses.failure,
                &[],
            ),
            (
                "an expansion names the path and the content",
                "NAME=out.txt\nWHAT=written\necho $WHAT > $NAME\ncat out.txt",
                statuses.success,
                &["written"],
            ),
            (
                "a redirected command this session has no utility for leaves the file empty",
                "mvn package > out.txt\ncat out.txt",
                statuses.success,
                &[],
            ),
        ];
        for (case, program, expected_status, expected_output) in rows {
            let mut held = session();
            let mut status = statuses.success;
            for line in program.lines() {
                status = held.run(line);
            }
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected status {expected_status}, got {status}"
            );
            assert_eq!(
                held.output(),
                expected_output,
                "case {case:?}: expected {expected_output:?}, got {:?}",
                held.output()
            );
        }
    }

    /// RULE: the tree is bounded like everything else — a write past the per-file ceiling,
    /// one past the whole tree's, and a file past the file count are each a write this
    /// session did not perform, and the file it would have written is not there.
    #[test]
    fn the_tree_s_bounds_stop_the_write() {
        let statuses = Statuses::default();

        // One file may hold no more than `file_bytes`.
        let mut held = ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                bounds: Bounds {
                    file_bytes: 8,
                    ..Bounds::default()
                },
                output: Some(OutputLimits {
                    lines: 8,
                    line_bytes: 64,
                }),
                ..SessionConfig::default()
            },
        );
        let status = held.run("echo 0123456789 > out.txt");
        assert_eq!(
            status, statuses.could_not_run,
            "case the per-file ceiling: expected {}, got {status}",
            statuses.could_not_run
        );
        assert_eq!(
            held.run("cat out.txt"),
            statuses.failure,
            "case the per-file ceiling: the file must not be there, got {:?}",
            held.output()
        );

        // The tree holds no more than `files` files.
        let mut held = ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                bounds: Bounds {
                    files: 1,
                    ..Bounds::default()
                },
                ..SessionConfig::default()
            },
        );
        assert_eq!(
            held.run("echo one > first.txt"),
            statuses.success,
            "case the file count: the first file is written"
        );
        assert_eq!(
            held.run("echo two > second.txt"),
            statuses.could_not_run,
            "case the file count: a file past the count is not written"
        );
        assert_eq!(
            held.run("echo again > first.txt"),
            statuses.success,
            "case the file count: a file the tree already holds is still written"
        );

        // The whole tree holds no more than `tree_bytes`.
        let mut held = ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                bounds: Bounds {
                    tree_bytes: 24,
                    ..Bounds::default()
                },
                ..SessionConfig::default()
            },
        );
        assert_eq!(
            held.run("echo onetwothree > first.txt"),
            statuses.success,
            "case the tree ceiling: the first file fits"
        );
        assert_eq!(
            held.run("echo onetwothree > second.txt"),
            statuses.could_not_run,
            "case the tree ceiling: the write past it is not performed"
        );
    }

    /// RULE: a variable the program sets is what its parameter expansions resolve against,
    /// an unset parameter expands to nothing, and `unset` removes one.
    #[test]
    fn variables_are_the_session_s_own() {
        // (case, the program, the line it wrote)
        let rows: [(&str, &str, &str); 5] = [
            ("an assignment and an expansion", "A=one\necho $A", "one"),
            ("export sets a variable", "export B=two\necho ${B}", "two"),
            ("an unset parameter", "echo [${ABSENT}]", "[]"),
            (
                "a default value",
                "echo ${ABSENT:-fallback}",
                "fallback",
            ),
            ("unset removes it", "C=three\nunset C\necho [$C]", "[]"),
        ];
        for (case, program, expected) in rows {
            let held = run(program);
            let written = held.output().last().cloned().unwrap_or_default();
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: `exit` ends the session with the status it names, and nothing after it is
    /// performed.
    #[test]
    fn exit_ends_the_session() {
        for (case, program, expected_status, expected_lines) in [
            ("exit with no status", "echo one\nexit\necho two", 0, 1),
            ("exit 3", "echo one\nexit 3\necho two", 3, 1),
            ("exit past 255 wraps", "exit 300", 44, 0),
        ] {
            let held = run(program);
            assert_eq!(
                held.exit_status(),
                Some(expected_status),
                "case {case:?}: expected the session to end with {expected_status}, got {:?}",
                held.exit_status()
            );
            assert_eq!(
                held.output().len(),
                expected_lines,
                "case {case:?}: expected {expected_lines} line(s), got {:?}",
                held.output()
            );
        }
    }

    /// RULE: `$(…)` is performed by this session — over the same catalog, the same
    /// environment programs and the same budget — and what it wrote, with its trailing
    /// newlines removed, is the word. It is a subshell: what it sets and an `exit` it
    /// performs do not reach the session that substituted it. The status of a command with
    /// no command name is the last substitution's; every other command answers for itself.
    #[test]
    fn command_substitution_is_performed_in_a_subshell() {
        let statuses = Statuses::default();
        // (case, the program, the line it wrote last)
        let written: [(&str, &str, &str); 7] = [
            ("output becomes the word", "A=$(echo one)\necho [$A]", "[one]"),
            (
                "trailing newlines are removed, inner ones kept",
                "A=$(echo one; echo two)\necho \"[$A]\"",
                "[one\ntwo]",
            ),
            (
                "a substitution nested in a substitution",
                "A=$(echo $(echo deep))\necho [$A]",
                "[deep]",
            ),
            (
                "a substituted command the session does not perform substitutes nothing",
                "A=$(whoami)\necho [$A]",
                "[]",
            ),
            (
                "the variables it sets are its own",
                "export KEPT=outer\nA=$(export KEPT=inner)\necho [$KEPT]",
                "[outer]",
            ),
            (
                "and so are the ones it removes",
                "export KEPT=outer\nA=$(unset KEPT)\necho [$KEPT]",
                "[outer]",
            ),
            (
                "the backquoted spelling of the same substitution",
                "A=`echo one`\necho [$A]",
                "[one]",
            ),
        ];
        for (case, program, expected) in written {
            let held = run(program);
            // What the program wrote, as the one text it wrote: a value carrying a newline is
            // kept as two lines of the session's output, and it is the TEXT the word became
            // that each row is about.
            let wrote = held.output().join("\n");
            assert_eq!(
                wrote, expected,
                "case {case:?}: expected {expected:?}, got {wrote:?}"
            );
        }

        // An `exit` inside a substitution ends that subshell alone: the session goes on.
        let held = run("A=$(exit 3)\necho after");
        assert_eq!(
            held.exit_status(),
            None,
            "case \"an exit inside a substitution\": expected the session to go on, got {:?}",
            held.exit_status()
        );
        assert_eq!(
            held.output(),
            ["after"],
            "case \"an exit inside a substitution\": expected the next line to be performed, \
             got {:?}",
            held.output()
        );

        // (case, the line, the status it answers)
        let answered: [(&str, String, i32); 5] = [
            (
                "a command with a name of its own answers for itself",
                "echo $(false)".to_string(),
                statuses.success,
            ),
            (
                "a command with no name answers with its substitution's status",
                "A=$(false)".to_string(),
                statuses.failure,
            ),
            (
                "… and with its LAST substitution's status",
                "A=$(false) B=$(true)".to_string(),
                statuses.success,
            ),
            (
                "… including a substituted command the session does not perform",
                "A=$(whoami)".to_string(),
                statuses.could_not_run,
            ),
            (
                "a word nesting more substitutions than one word may",
                format!("A={}true{}", "$(".repeat(12), ")".repeat(12)),
                statuses.could_not_run,
            ),
        ];
        for (case, line, expected) in answered {
            let mut held = session();
            let status = held.run(&line);
            assert_eq!(
                status, expected,
                "case {case:?}: expected status {expected}, got {status}"
            );
        }

        // The substituted commands come out of the SAME command budget, and performing a
        // substitution AT ALL spends one of it, so neither the commands inside one nor the
        // number of them a line opens can outrun the budget.
        let two_commands = SessionConfig {
            bounds: Bounds {
                commands: 2,
                ..Bounds::default()
            },
            output: Some(OutputLimits {
                lines: 8,
                line_bytes: 64,
            }),
            ..SessionConfig::default()
        };
        let mut held = ShellSession::new(BTreeMap::new(), two_commands);
        held.run("A=$(echo one)");
        held.run("echo [$A]");
        assert!(
            held.output().is_empty(),
            "case \"the shared command budget\": expected the budget to be spent by the \
             substitution, got {:?}",
            held.output()
        );

        // A substitution whose text performs no command at all still spends one, so a line
        // carrying many of them is bounded by the same budget.
        // (case, the whole command budget, the line, the status it answers)
        for (case, commands, line, expected) in [
            (
                "a budget the command and its substitution spend between them",
                2,
                "A=$(true)",
                statuses.could_not_run,
            ),
            (
                "a budget for the command, the substitution and the command inside it",
                3,
                "A=$(true)",
                statuses.success,
            ),
            (
                "a second substitution on the same line, past the budget",
                3,
                "echo $(true) $(true)",
                statuses.could_not_run,
            ),
        ] {
            let mut held = ShellSession::new(
                BTreeMap::new(),
                SessionConfig {
                    bounds: Bounds {
                        commands,
                        ..Bounds::default()
                    },
                    ..SessionConfig::default()
                },
            );
            let status = held.run(line);
            assert_eq!(
                status, expected,
                "case {case:?}: expected status {expected}, got {status}"
            );
        }

        // The variables a session STARTS with are held to the store's own ceilings, so what
        // its caller hands it cannot grow the session past them.
        let handed: BTreeMap<String, String> = ["A", "B", "C"]
            .into_iter()
            .map(|name| (name.to_string(), "value".to_string()))
            .collect();
        let held = ShellSession::new(
            handed,
            SessionConfig {
                bounds: Bounds {
                    session_variables: 2,
                    ..Bounds::default()
                },
                ..SessionConfig::default()
            },
        );
        let kept = ["A", "B", "C"]
            .into_iter()
            .filter(|name| held.variable(name).is_some())
            .count();
        assert_eq!(
            kept, 2,
            "case \"the variables a session starts with\": expected the store's ceiling to \
             hold for them too, got {kept} of them"
        );
    }

    /// An environment that provides ONE program, so a session's catalog is not all it can
    /// name — and that program answers only the one invocation it is given.
    #[derive(Debug)]
    struct OneProgram;

    impl EnvironmentPrograms for OneProgram {
        fn run(
            &self,
            name: &str,
            arguments: &[String],
            _files: &mut dyn ProgramFiles,
        ) -> Option<ProgramOutcome> {
            let queried: Vec<&str> = arguments.iter().map(String::as_str).collect();
            (name == "report" && queried == ["--version"]).then(|| ProgramOutcome {
                status: 0,
                output: vec!["report 1.2.3".to_string()],
            })
        }
    }

    /// An environment whose one program CARRIES a file: `carry <from> <to>` moves what the
    /// session's tree holds at one path to the other, which is the whole of what a program
    /// that takes a file somewhere does to the tree.
    #[derive(Debug)]
    struct CarryingProgram;

    impl EnvironmentPrograms for CarryingProgram {
        fn run(
            &self,
            name: &str,
            arguments: &[String],
            files: &mut dyn ProgramFiles,
        ) -> Option<ProgramOutcome> {
            if name != "carry" || arguments.len() != 2 {
                return None;
            }
            let Some(carried) = files.read(&arguments[0]) else {
                return Some(ProgramOutcome {
                    status: 1,
                    output: vec![format!("carry: {}: no such file", arguments[0])],
                });
            };
            if !files.write(&arguments[1], &carried) {
                return Some(ProgramOutcome {
                    status: 1,
                    output: vec![format!("carry: {}: not written", arguments[1])],
                });
            }
            files.remove(&arguments[0]);
            Some(ProgramOutcome {
                status: 0,
                output: vec![format!("carry: {} to {}", arguments[0], arguments[1])],
            })
        }
    }

    /// RULE: a program of the environment reads, writes and removes the FILES of the session
    /// it was invoked in, so a file the commands around it wrote is the one it carries, a file
    /// it leaves is one they read back, and a file it took away is gone — and a path the tree
    /// holds nothing at is the program's own failure.
    #[test]
    fn an_environments_program_carries_the_sessions_own_files() {
        /// One row: the program, the status it answers, and what the tree then holds at the
        /// destination and at the source.
        struct Row {
            case: &'static str,
            program: &'static str,
            status: i32,
            destination_holds: Option<&'static str>,
            source_holds: Option<&'static str>,
        }

        let statuses = Statuses::default();
        let rows = [
            Row {
                case: "a file a redirection left behind",
                program: "echo carried > /tmp/source && carry /tmp/source /tmp/destination",
                status: statuses.success,
                destination_holds: Some("carried\n"),
                source_holds: None,
            },
            Row {
                case: "a file the program left, read back by the catalog",
                program: "echo carried > /tmp/source; carry /tmp/source /tmp/destination; \
                          cat /tmp/destination > /tmp/read-back",
                status: statuses.success,
                destination_holds: Some("carried\n"),
                source_holds: None,
            },
            Row {
                case: "a source the tree holds nothing at",
                program: "echo kept > /tmp/source; carry /tmp/absent /tmp/destination",
                status: statuses.failure,
                destination_holds: None,
                source_holds: Some("kept\n"),
            },
            Row {
                case: "an invocation this environment performs nothing of",
                program: "echo kept > /tmp/source; carry /tmp/source",
                status: statuses.could_not_run,
                destination_holds: None,
                source_holds: Some("kept\n"),
            },
        ];
        for Row {
            case,
            program,
            status: expected_status,
            destination_holds,
            source_holds,
        } in rows
        {
            let mut session = ShellSession::with_programs(
                BTreeMap::new(),
                SessionConfig::default(),
                Box::new(CarryingProgram),
            );
            let status = session.run(program);
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected status {expected_status}, got {status}"
            );
            let stored = session.tree_files();
            let held = |path: &str| {
                stored
                    .iter()
                    .find(|(held, _)| *held == path)
                    .map(|(_, contents)| *contents)
            };
            let destination = held("/tmp/destination");
            assert_eq!(
                destination, destination_holds,
                "case {case:?}: expected the destination to hold {destination_holds:?}, got \
                 {destination:?}"
            );
            let source = held("/tmp/source");
            assert_eq!(
                source, source_holds,
                "case {case:?}: expected the source to hold {source_holds:?}, got {source:?}"
            );
            // The read-back row proves the catalog's own `cat` reads the file the program
            // left, over the same tree.
            if case.contains("read back") {
                let read_back = held("/tmp/read-back");
                assert_eq!(
                    read_back, destination_holds,
                    "case {case:?}: expected `cat` to read the carried file back, got \
                     {read_back:?}"
                );
            }
        }
    }

    /// RULE: a command the catalog does not name is asked of the programs the ENVIRONMENT
    /// provides, and what that program answered — its status and the lines it wrote — is what
    /// the command answers; a name, or an invocation, the environment provides no program
    /// for is still a command that could not be run.
    #[test]
    fn the_environments_programs_answer_beside_the_catalog() {
        let statuses = Statuses::default();
        let configured = SessionConfig {
            output: Some(OutputLimits {
                lines: 8,
                line_bytes: 64,
            }),
            ..SessionConfig::default()
        };
        // (case, the line, the status it answers, the lines it wrote)
        let rows: [(&str, &str, i32, &[&str]); 4] = [
            (
                "the invocation the environment provides",
                "report --version",
                statuses.success,
                &["report 1.2.3"],
            ),
            (
                "the same program, invoked another way",
                "report --help",
                statuses.could_not_run,
                &[],
            ),
            (
                "a program this environment provides none of",
                "other --version",
                statuses.could_not_run,
                &[],
            ),
            (
                "the catalog's own utilities still answer first",
                "echo one",
                statuses.success,
                &["one"],
            ),
        ];
        for (case, line, expected_status, expected_output) in rows {
            let mut held =
                ShellSession::with_programs(BTreeMap::new(), configured, Box::new(OneProgram));
            let status = held.run(line);
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected status {expected_status}, got {status}"
            );
            assert_eq!(
                held.output(),
                expected_output,
                "case {case:?}: expected {expected_output:?}, got {:?}",
                held.output()
            );
        }

        // What such a program wrote is substitutable, like any other command's output.
        let mut held =
            ShellSession::with_programs(BTreeMap::new(), configured, Box::new(OneProgram));
        held.run("V=$(report --version)");
        assert_eq!(
            held.variable("V"),
            Some("report 1.2.3"),
            "case \"a substituted environment program\": expected its line as the word, got {:?}",
            held.variable("V")
        );
    }

    /// RULE: every bound is the session's own — the command budget, the line size, the
    /// nesting count, the variable count and the output ceilings each stop the work.
    #[test]
    fn every_bound_stops_the_work() {
        let statuses = Statuses::default();

        // The command budget is latched: past it a line performs nothing.
        let mut held = ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                bounds: Bounds {
                    commands: 2,
                    ..Bounds::default()
                },
                output: Some(OutputLimits {
                    lines: 8,
                    line_bytes: 64,
                }),
                ..SessionConfig::default()
            },
        );
        for _ in 0..4 {
            held.run("echo line");
        }
        assert_eq!(
            held.output().len(),
            2,
            "case the command budget: expected 2 performed commands, got {:?}",
            held.output()
        );

        // A line past the size bound, and one past the nesting bound, are not read at all.
        let mut held = session();
        let long = format!("echo {}", "x".repeat(Bounds::default().command_bytes));
        assert_eq!(
            held.run(&long),
            statuses.could_not_run,
            "case an oversized line: expected it to perform nothing"
        );
        let nested = format!("echo {}", "(".repeat(Bounds::default().nesting + 1));
        assert_eq!(
            held.run(&nested),
            statuses.could_not_run,
            "case a deeply nested line: expected it to perform nothing"
        );
        assert!(
            held.output().is_empty(),
            "case the parse bounds: expected no output, got {:?}",
            held.output()
        );

        // The variable count is bounded: a further assignment performs nothing.
        let mut held = ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                bounds: Bounds {
                    session_variables: 1,
                    ..Bounds::default()
                },
                ..SessionConfig::default()
            },
        );
        assert_eq!(
            held.run("A=one"),
            statuses.success,
            "case the first variable: expected it to be set"
        );
        assert_eq!(
            held.run("B=two"),
            statuses.could_not_run,
            "case the variable ceiling: expected the assignment to perform nothing"
        );

        // The output ceilings: at most `lines` entries, each at most `line_bytes` bytes.
        let mut held = ShellSession::new(
            BTreeMap::new(),
            SessionConfig {
                output: Some(OutputLimits {
                    lines: 2,
                    line_bytes: 4,
                }),
                ..SessionConfig::default()
            },
        );
        for _ in 0..5 {
            held.run("echo abcdefgh");
        }
        assert_eq!(
            held.output(),
            ["abcd", "abcd"],
            "case the output ceilings: expected two truncated lines, got {:?}",
            held.output()
        );

        // A session that keeps no output keeps none, whatever the program writes.
        let mut held = ShellSession::new(BTreeMap::new(), SessionConfig::default());
        held.run("echo nothing-is-kept");
        assert!(
            held.output().is_empty(),
            "case no output limits: expected nothing kept, got {:?}",
            held.output()
        );
    }

    /// A session that runs in a tree holding a counter, a sealed identity, a console device
    /// and a null device — the shape of a machine a boot script runs on.
    fn in_volume() -> ShellSession {
        let mut volume = ShellVolume::default();
        volume.put_file("/var/lib/counter", "1\n");
        volume.seal_file(
            "/proc/sys/kernel/random/boot_id",
            "6e3f1f2a-0f4d-4b7f-9a2e-7c1d5e8b4a30\n",
        );
        volume.add_keeping_device("/dev/console");
        volume.add_discarding_device("/dev/null");
        ShellSession::in_volume(
            BTreeMap::new(),
            SessionConfig {
                output: Some(OutputLimits {
                    lines: 16,
                    line_bytes: 256,
                }),
                ..SessionConfig::default()
            },
            volume,
        )
    }

    /// RULE: a session that runs in a volume performs the forms a program of a machine is
    /// written in — the file utilities, redirections, command substitution and arithmetic
    /// expansion — over that tree and nothing else.
    #[test]
    fn a_session_in_a_volume_performs_a_machine_s_program() {
        let statuses = Statuses::default();
        // (case, the program, the status, the lines it wrote to the console device)
        let rows: [(&str, &str, i32, &[&str]); 9] = [
            (
                "a redirection to a device",
                "echo marker > /dev/console",
                statuses.success,
                &["marker"],
            ),
            (
                "a command substitution reading a sealed file",
                "echo id $(cat /proc/sys/kernel/random/boot_id) > /dev/console",
                statuses.success,
                &["id 6e3f1f2a-0f4d-4b7f-9a2e-7c1d5e8b4a30"],
            ),
            (
                "an arithmetic expansion over a file's contents",
                "C=$(cat /var/lib/counter)\necho next $((C + 1)) > /dev/console",
                statuses.success,
                &["next 2"],
            ),
            (
                "a file written and read back",
                "echo kept > /var/lib/other\ncat /var/lib/other > /dev/console",
                statuses.success,
                &["kept"],
            ),
            (
                "an appending redirection",
                "echo one >> /dev/console\necho two >> /dev/console",
                statuses.success,
                &["one", "two"],
            ),
            (
                "a failure reported on standard error",
                "cat /var/lib/absent 2>/dev/console",
                statuses.failure,
                &["cat: /var/lib/absent: No such file or directory"],
            ),
            (
                "a discarded failure",
                "cat /var/lib/absent 2>/dev/null",
                statuses.failure,
                &[],
            ),
            (
                "a question about the tree",
                "test -f /var/lib/counter && echo there > /dev/console",
                statuses.success,
                &["there"],
            ),
            (
                "a program run through sh -c",
                "sh -c 'printf \"%s=%d\\n\" count 7 > /dev/console'",
                statuses.success,
                &["count=7"],
            ),
        ];
        for (case, program, expected_status, expected_console) in rows {
            let mut held = in_volume();
            let mut status = statuses.success;
            for line in program.lines() {
                status = held.run(line);
            }
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected status {expected_status}, got {status}"
            );
            let volume = held.into_volume().expect("the session ran in a volume");
            assert_eq!(
                volume.device_lines("/dev/console"),
                expected_console,
                "case {case:?}: expected the console to carry {expected_console:?}, got {:?}",
                volume.device_lines("/dev/console")
            );
        }
    }

    /// RULE: `tee` writes what it reads into each file it names and passes it on, from the
    /// start or appended, and a session given no tree performs none of it.
    #[test]
    fn tee_writes_what_it_reads_into_the_files_it_names() {
        let statuses = Statuses::default();
        let mut held = in_volume();
        assert_eq!(
            held.run("echo first | tee /var/lib/page"),
            statuses.success,
            "expected the write to be taken"
        );
        assert_eq!(
            held.run("echo second | tee -a /var/lib/page > /dev/console"),
            statuses.success,
            "expected the appending write to be taken"
        );
        let volume = held.into_volume().expect("the session ran in a volume");
        assert_eq!(
            volume.read("/var/lib/page").as_deref(),
            Some("first\nsecond\n"),
            "expected the file to hold both writes, got {:?}",
            volume.read("/var/lib/page")
        );
        // What it read is passed on, which is what the redirection above collected.
        assert_eq!(
            volume.device_lines("/dev/console"),
            ["second"],
            "expected the input to be passed on, got {:?}",
            volume.device_lines("/dev/console")
        );
    }

    /// RULE: a HERE-DOCUMENT is the command's standard input, written inside the command
    /// itself — taken literally for a quoted delimiter, expanded for an unquoted one, with
    /// `<<-` stripping the leading tabs — and a here-string is the one word it names.
    #[test]
    fn a_here_document_is_the_commands_standard_input() {
        let statuses = Statuses::default();
        // (case, the program, the contents the file then holds)
        let rows: &[(&str, &str, &str)] = &[
            (
                "a quoted delimiter takes the document literally",
                "cat > /var/lib/page <<'EOF'\nMARKER $HOME\nEOF",
                "MARKER $HOME\n",
            ),
            (
                "an unquoted delimiter expands the document",
                "NAME=marker\ncat > /var/lib/page <<EOF\nvalue $NAME\nEOF",
                "value marker\n",
            ),
            (
                "the tab-stripping form",
                "cat > /var/lib/page <<-'EOF'\n\tindented\nEOF",
                "indented\n",
            ),
            (
                "a here-string",
                "cat > /var/lib/page <<<'one word'",
                "one word\n",
            ),
            (
                "a document of several lines",
                "cat > /var/lib/page <<'EOF'\nfirst\nsecond\nEOF",
                "first\nsecond\n",
            ),
        ];
        for (case, program, expected) in rows {
            let mut held = in_volume();
            let status = held.run(program);
            assert_eq!(
                status, statuses.success,
                "case {case:?}: expected the command to run, got {status}"
            );
            let volume = held.into_volume().expect("the session ran in a volume");
            let held = volume.read("/var/lib/page");
            assert_eq!(
                held.as_deref(),
                Some(*expected),
                "case {case:?}: expected the file to hold {expected:?}, got {held:?}"
            );
        }

        // A session given no tree performs none of those forms.
        let mut own = session();
        assert_eq!(
            own.run("cat <<'EOF'\nnothing\nEOF"),
            statuses.could_not_run,
            "a session with no tree performs no here-document"
        );
    }

    /// RULE: what a program wrote to the tree is what the caller takes back out, so the next
    /// run of a program on that machine reads what this one wrote — which is how a boot
    /// script keeps a count across boots.
    #[test]
    fn what_a_program_wrote_to_the_tree_comes_back_out() {
        let program = [
            "COUNT=$(cat /var/lib/counter)",
            "COUNT=$((COUNT + 1))",
            "echo $COUNT > /var/lib/counter",
            "echo boot $COUNT > /dev/console",
        ];
        let mut carried: Option<ShellVolume> = None;
        // (the boot, the count the console should carry)
        for (boot, expected) in [(1_u32, "boot 2"), (2, "boot 3")] {
            let mut held = match carried.take() {
                Some(volume) => ShellSession::in_volume(
                    BTreeMap::new(),
                    SessionConfig {
                        output: Some(OutputLimits {
                            lines: 16,
                            line_bytes: 256,
                        }),
                        ..SessionConfig::default()
                    },
                    volume,
                ),
                None => in_volume(),
            };
            for line in program {
                held.run(line);
            }
            let volume = held.into_volume().expect("the session ran in a volume");
            let console = volume.device_lines("/dev/console").to_vec();
            assert_eq!(
                console,
                [expected],
                "case run {boot}: expected the console to carry {expected:?}, got {console:?}"
            );
            // The next run starts from the tree this one left, with its console cleared the
            // way a device of a new boot is.
            let mut next = ShellVolume::default();
            for (path, contents) in volume.stored_files() {
                next.put_file(path, contents);
            }
            next.seal_file(
                "/proc/sys/kernel/random/boot_id",
                "6e3f1f2a-0f4d-4b7f-9a2e-7c1d5e8b4a30\n",
            );
            next.add_keeping_device("/dev/console");
            next.add_discarding_device("/dev/null");
            carried = Some(next);
        }
    }

    /// RULE: every bound a session in a volume adds is the emulator's own — how many
    /// substitutions one session performs, how deeply they nest, how many bytes one of them
    /// may capture, and the bound an expansion's own TEXT is read under before it reaches a
    /// grammar — and a program past one of them performs nothing rather than more.
    #[test]
    fn every_volume_bound_stops_the_work() {
        let statuses = Statuses::default();
        let config = |bounds: Bounds| SessionConfig {
            bounds,
            output: Some(OutputLimits {
                lines: 8,
                line_bytes: 64,
            }),
            ..SessionConfig::default()
        };
        let volume = || {
            let mut volume = ShellVolume::default();
            volume.put_file("/var/lib/counter", "1\n");
            volume.add_keeping_device("/dev/console");
            volume
        };

        // The substitution budget: past it a word carrying one has no expansion, so the
        // command is not performed.
        let mut held = ShellSession::in_volume(
            BTreeMap::new(),
            config(Bounds {
                substitutions: 1,
                ..Bounds::default()
            }),
            volume(),
        );
        assert_eq!(
            held.run("echo $(cat /var/lib/counter) > /dev/console"),
            statuses.success,
            "case the first substitution: expected it to be performed"
        );
        assert_eq!(
            held.run("echo $(cat /var/lib/counter) > /dev/console"),
            statuses.could_not_run,
            "case the substitution budget: expected the command to perform nothing"
        );

        // The capture ceiling: a substitution whose output goes past it has no value.
        let mut held = ShellSession::in_volume(
            BTreeMap::new(),
            config(Bounds {
                substitution_bytes: 4,
                ..Bounds::default()
            }),
            volume(),
        );
        assert_eq!(
            held.run("echo $(echo abcdefghijkl) > /dev/console"),
            statuses.could_not_run,
            "case the capture ceiling: expected the command to perform nothing"
        );

        // The nesting ceiling: a substitution inside a substitution is performed while the
        // nesting is inside the bound, and past it the inner one has no value at all, so
        // the word carrying it expands without what that command would have written.
        // (case, the depth allowed, what the console then carries)
        for (case, depth, expected) in [("inside the depth", 2, "1"), ("past the depth", 1, "")] {
            let mut held = ShellSession::in_volume(
                BTreeMap::new(),
                config(Bounds {
                    substitution_depth: depth,
                    ..Bounds::default()
                }),
                volume(),
            );
            held.run("echo $(echo $(cat /var/lib/counter)) > /dev/console");
            let written = held.into_volume().expect("a volume").device_lines("/dev/console").to_vec();
            assert_eq!(
                written,
                [expected],
                "case {case:?}: expected the console to carry {expected:?}, got {written:?}"
            );
        }

        // The bound an ARITHMETIC expansion's own text is read under: the expansion of a
        // variable can materialise text far more deeply nested than the line itself, and a
        // parser costs a level of recursion per nested group, so the expansion has no value
        // past the bound rather than driving that recursion.
        let mut held = ShellSession::in_volume(
            BTreeMap::from([("NESTED".to_string(), "(".repeat(64))]),
            config(Bounds {
                nesting: 8,
                ..Bounds::default()
            }),
            volume(),
        );
        assert_eq!(
            held.run("echo $(( $NESTED 1 )) > /dev/console"),
            statuses.could_not_run,
            "case the expansion's own nesting: expected the command to perform nothing"
        );
        assert_eq!(
            held.run("echo $(( 1 + 1 )) > /dev/console"),
            statuses.success,
            "case an expansion inside the bound: expected it to be performed"
        );
        let written = held
            .into_volume()
            .expect("a volume")
            .device_lines("/dev/console")
            .to_vec();
        assert_eq!(
            written,
            ["2"],
            "case the expansion's own nesting: expected only the bounded expansion's value \
             on the console, got {written:?}"
        );
    }

    /// RULE: a session that was given no MACHINE's tree performs none of the forms a machine's
    /// program is written in — there is no device to write, no descriptor to report on and no
    /// storage of the machine to read — so what such a session performs is its own catalog
    /// over its own tree. (A command SUBSTITUTION is not one of those forms: it is the
    /// shell's own, performed by every session over whatever that session performs, which
    /// `command_substitution_is_performed_in_a_subshell` is the subject of.)
    #[test]
    fn a_session_with_no_volume_performs_none_of_them() {
        let statuses = Statuses::default();
        // (case, the line a program of a machine is written with)
        let rows = [
            ("an arithmetic expansion", "echo $((1 + 1))"),
            ("a redirection reading a file", "cat < out.txt"),
            ("a redirection of standard error", "cat missing 2> err.txt"),
            ("a question about a tree", "test -f out.txt"),
            ("a nested program", "sh -c 'echo inside'"),
            ("making a directory", "mkdir -p /var/lib/sim"),
            ("removing a file", "rm -f out.txt"),
            ("writing a formatted line", "printf '%s\\n' value"),
        ];
        for (case, line) in rows {
            let mut held = session();
            let status = held.run(line);
            assert_eq!(
                status, statuses.could_not_run,
                "case {case:?}: expected the status a shell answers for a command it cannot \
                 run, got {status}"
            );
            assert!(
                held.output().is_empty(),
                "case {case:?}: expected nothing written, got {:?}",
                held.output()
            );
        }
    }

    /// A session whose own tree is the WORKING tree of its environment, which is what a
    /// caller holding storage of its own hands its program.
    fn working_tree_session() -> ShellSession {
        session().with_file_utilities()
    }

    /// RULE: a session working over its own WORKING tree performs the file utilities over
    /// that tree — where a path is a key, so a directory of it exists exactly as long as a
    /// file under it does — and the arithmetic its program computes with.
    ///
    /// The rows differ along what the rule generalises over: which utility is named, whether
    /// the tree already holds the path, how the path is spelled, and what the arithmetic
    /// comes to.
    #[test]
    fn a_session_over_a_working_tree_performs_the_file_utilities() {
        let statuses = Statuses::default();
        // (case, the line, the status it answers)
        let rows: [(&str, &str, i32); 12] = [
            ("a formatted line", "printf '%s' value", statuses.success),
            ("a directory, which the tree needs none of", "mkdir -p dist", statuses.success),
            ("a file written under it", "echo built > dist/app.txt", statuses.success),
            ("the file read back", "cat dist/app.txt", statuses.success),
            ("a question about that file", "test -f dist/app.txt", statuses.success),
            (
                "a question about the directory above it",
                "test -d dist",
                statuses.success,
            ),
            ("a question about a path nothing wrote", "test -f missing.txt", statuses.failure),
            ("the file touched again", "touch dist/app.txt", statuses.success),
            ("the file removed", "rm dist/app.txt", statuses.success),
            ("the same file removed twice", "rm dist/app.txt", statuses.failure),
            ("a removal that passes over it", "rm -f dist/app.txt", statuses.success),
            ("a nested program", "sh -c 'echo inside'", statuses.success),
        ];
        let mut held = working_tree_session();
        for (case, line, expected) in rows {
            let status = held.run(line);
            assert_eq!(
                status, expected,
                "case {case:?}: expected the status {expected}, got {status}"
            );
        }
        let mut computing = working_tree_session();
        assert_eq!(
            computing.run("export SEED=6"),
            statuses.success,
            "case \"the value the arithmetic reads\": the assignment must be performed"
        );
        assert_eq!(
            computing.run("export RESULT=\"$(( SEED * 7 + 3 ))\""),
            statuses.success,
            "case \"the arithmetic\": a session over a working tree performs the expansion"
        );
        assert_eq!(
            computing.variable("RESULT"),
            Some("45"),
            "case \"the arithmetic\": expected what the expression comes to, got {:?}",
            computing.variable("RESULT")
        );
    }

    /// RULE: the environment SEEDS the files its program is to find in the working tree, and
    /// takes back out what the program left there — which is how a service hands a run its
    /// input and keeps its output.
    #[test]
    fn the_working_trees_files_go_in_and_come_back_out() {
        let mut held = working_tree_session();
        assert!(
            held.put_file("payload.txt", "SEEDED"),
            "the tree must take the file the environment seeded"
        );
        assert_eq!(
            held.run("export READ=\"$(cat payload.txt)\""),
            Statuses::default().success,
            "the program must read the seeded file"
        );
        assert_eq!(
            held.variable("READ"),
            Some("SEEDED"),
            "expected the seeded bytes, got {:?}",
            held.variable("READ")
        );
        held.run("echo produced > out.txt");
        let files = held.tree_files();
        assert_eq!(
            files,
            [("out.txt", "produced\n"), ("payload.txt", "SEEDED")],
            "expected the seeded file and the one the program wrote, got {files:?}"
        );
    }

    /// RULE: a session working over a tree performs the grammar's own control flow around the
    /// commands inside it — `if`/`elif`/`else`, `while`, `until`, `for` and a brace group —
    /// and the forms that need an environment of their own are still ones it does not
    /// perform. A session with no tree performs none of them.
    ///
    /// The rows differ along what the rule generalises over: the form, whether its condition
    /// holds, how many times a body runs, and whether a command inside it failed.
    #[test]
    fn a_session_over_a_tree_performs_the_grammars_control_flow() {
        let statuses = Statuses::default();
        // (case, the program, the status it answers, what it left in STATE)
        let rows: [(&str, &str, i32, &str); 9] = [
            ("an if that holds", "if true; then export STATE=then; fi", statuses.success, "then"),
            (
                "an if that does not, with an else",
                "if false; then export STATE=then; else export STATE=else; fi",
                statuses.success,
                "else",
            ),
            (
                "an elif",
                "if false; then export STATE=then; elif true; then export STATE=elif; fi",
                statuses.success,
                "elif",
            ),
            (
                "an if no branch of which runs",
                "if false; then export STATE=then; fi",
                statuses.success,
                "",
            ),
            (
                "a while loop",
                "export STATE=0; while [ $STATE -lt 3 ]; do export STATE=\"$(( STATE + 1 ))\"; done",
                statuses.success,
                "3",
            ),
            (
                "an until loop",
                "export STATE=0; until [ $STATE -ge 2 ]; do export STATE=\"$(( STATE + 1 ))\"; done",
                statuses.success,
                "2",
            ),
            (
                "a for loop over its words",
                "for WORD in a b c; do export STATE=\"$WORD\"; done",
                statuses.success,
                "c",
            ),
            ("a brace group", "{ export STATE=grouped; }", statuses.success, "grouped"),
            (
                "a command inside that failed",
                "if true; then export STATE=entered; false; fi",
                statuses.failure,
                "entered",
            ),
        ];
        for (case, program, expected, state) in rows {
            let mut held = working_tree_session();
            let status = held.run(program);
            assert_eq!(
                status, expected,
                "case {case:?}: expected the status {expected}, got {status}"
            );
            assert_eq!(
                held.variable("STATE").unwrap_or_default(),
                state,
                "case {case:?}: expected STATE {state:?}, got {:?}",
                held.variable("STATE")
            );
            // The same program in a session with no tree of its own performs nothing.
            let mut without = session();
            assert_eq!(
                without.run(program),
                statuses.could_not_run,
                "case {case:?}: a session with no tree must perform no compound command"
            );
        }
        // A loop with no end of its own ends, whatever its condition and its body are made
        // of: every command spends one of the session's budget, a form this session does not
        // perform included, so no iteration is free.
        // (case, the program)
        let endless = [
            ("built from simple commands", "while true; do : ; done"),
            ("a condition and a body of subshells", "until ( : ); do ( : ); done"),
            (
                "a condition this session does not perform",
                "until case x in esac; do ( : ); done",
            ),
            (
                "a condition that is itself a loop",
                "while for i; do :; done; do ( : ); done",
            ),
            (
                "a condition no branch of which runs",
                "while if false; then :; fi; do ( : ); done",
            ),
        ];
        for (case, program) in endless {
            let mut held = ShellSession::new(
                BTreeMap::new(),
                SessionConfig {
                    bounds: Bounds {
                        commands: 16,
                        ..Bounds::default()
                    },
                    ..SessionConfig::default()
                },
            )
            .with_file_utilities();
            assert_eq!(
                held.run(program),
                statuses.could_not_run,
                "case {case:?}: expected the budget to end the loop"
            );
        }
    }

    /// RULE: the status an `exit` ended a program with is what the session reports until the
    /// caller that hands this session SEVERAL programs over one tree says the next one
    /// starts, and the commands of that next program are then performed.
    #[test]
    fn a_cleared_exit_lets_the_next_program_run() {
        let mut held = working_tree_session();
        held.run("exit 7");
        assert_eq!(
            held.exit_status(),
            Some(7),
            "the program ended with the status it named"
        );
        held.clear_exit();
        assert_eq!(
            held.exit_status(),
            None,
            "the session no longer carries the status the ended program left"
        );
        held.run("export AFTER=performed");
        assert_eq!(
            held.variable("AFTER"),
            Some("performed"),
            "the next program runs, got {:?}",
            held.variable("AFTER")
        );
        assert_eq!(
            held.exit_status(),
            None,
            "a program that ended no session leaves no status behind"
        );
    }

    /// RULE: `.` (and its `source` spelling) performs the commands of a file the tree holds
    /// in THIS session, so the assignments that file carries are the session's variables
    /// afterwards; a path the tree holds no file for is one the utility could not read, and
    /// a session with no tree of its own performs none of it.
    ///
    /// The rows differ along what the rule generalises over: the spelling of the utility,
    /// how the file's values are quoted, whether the file is there, and whether the
    /// variables are read back by a later command or by a command substitution.
    #[test]
    fn a_file_of_assignments_is_performed_in_the_session() {
        let statuses = Statuses::default();
        // (case, the spelling, the file's contents, the status it answers, what READ holds)
        let rows: [(&str, &str, &str, i32, &str); 5] = [
            (
                "a file of quoted assignments",
                ".",
                "NAME=\"Some Distribution\"\nID=\"somedist\"\n",
                statuses.success,
                "somedist",
            ),
            (
                "the same file through the bash spelling",
                "source",
                "ID=\"somedist\"\n",
                statuses.success,
                "somedist",
            ),
            (
                "an unquoted value",
                ".",
                "ID=somedist\nID_LIKE=other\n",
                statuses.success,
                "somedist",
            ),
            (
                "a value the file computes from the one above it",
                ".",
                "BASE=some\nID=\"${BASE}dist\"\n",
                statuses.success,
                "somedist",
            ),
            (
                "a file whose last assignment wins",
                ".",
                "ID=first\nID=somedist\n",
                statuses.success,
                "somedist",
            ),
        ];
        for (case, spelling, contents, expected, read) in rows {
            let mut held = working_tree_session();
            assert!(
                held.put_file("/etc/release-file", contents),
                "case {case:?}: the tree must take the file the environment published"
            );
            let status = held.run(&format!("{spelling} /etc/release-file"));
            assert_eq!(
                status, expected,
                "case {case:?}: expected the status {expected}, got {status}"
            );
            assert_eq!(
                held.variable("ID").unwrap_or_default(),
                read,
                "case {case:?}: expected ID {read:?}, got {:?}",
                held.variable("ID")
            );
            // The same file read inside a command substitution: what the sourced
            // assignments set is the WORD, and the subshell leaves the session as it was.
            let mut substituting = working_tree_session();
            assert!(
                substituting.put_file("/etc/release-file", contents),
                "case {case:?}: the tree must take the file the environment published"
            );
            assert_eq!(
                substituting.run(&format!(
                    "export READ=\"$({spelling} /etc/release-file && echo $ID)\""
                )),
                statuses.success,
                "case {case:?}: the substitution must be performed"
            );
            assert_eq!(
                substituting.variable("READ").unwrap_or_default(),
                read,
                "case {case:?}: expected READ {read:?}, got {:?}",
                substituting.variable("READ")
            );
            assert_eq!(
                substituting.variable("ID"),
                None,
                "case {case:?}: the subshell's own variables do not reach the session, got {:?}",
                substituting.variable("ID")
            );
        }
        // A path the tree holds no file for: the utility could not read it, and nothing was
        // performed.
        let mut missing = working_tree_session();
        assert_eq!(
            missing.run(". /etc/absent-file"),
            statuses.failure,
            "case \"a path the tree holds no file for\": expected the status for a file that \
             could not be read"
        );
        // A session with no tree of its own has no file to read commands from at all.
        let mut without = session();
        assert_eq!(
            without.run(". /etc/release-file"),
            statuses.could_not_run,
            "case \"a session with no tree\": the utility is one it does not perform"
        );
    }
}
