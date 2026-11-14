// SPDX-License-Identifier: CC0-1.0

//! The PROGRAMS the environment of a code interpreter session provides, beside the shell's
//! own utilities.
//!
//! A command line names two kinds of command: the shell's own utilities, which the shared
//! session performs (`service_common::posix_shell_session::BUILT_IN_COMMANDS` and
//! `VOLUME_COMMANDS`), and the programs the environment it runs in has installed — which
//! ones those are is the environment's fact, not the shell's. A code interpreter sandbox is
//! an environment with a filesystem of its own, so the programs it provides are the
//! READ-ONLY utilities that report on that filesystem, and these are they.
//!
//! Every one of them computes its answer from the session's own tree, through the files the
//! shared session hands it (`ProgramFiles`): there is no host process to start, no host
//! `PATH` to resolve against and no host file to open here, and a path is a key of that one
//! in-memory tree. A name this module provides no program for is answered as the shell
//! answers a command it could not run, so nothing is invented for a utility the environment
//! does not have.

use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

/// The programs this environment provides, as their names.
///
/// Each one REPORTS on the session's filesystem and changes nothing: `ls` lists a directory,
/// `cat`'s neighbours `head`, `tail` and `wc` read part of a file or count what is in it, and
/// `pwd` names the directory the program runs in.
pub const SANDBOX_PROGRAMS: &[&str] = &["ls", "head", "tail", "wc", "pwd"];

/// The status a utility answers when it "could not access" the path it was given, which is
/// what `ls`, `wc`, `head` and `tail` exit with for a path their filesystem holds nothing at.
const CANNOT_ACCESS: i32 = 2;

/// How many lines `head` and `tail` answer when the call names no count, which is the count
/// those utilities are specified to default to.
const DEFAULT_LINE_COUNT: usize = 10;

/// The directory a command of a code interpreter session runs in: the root of the session's
/// own workspace, which is the whole of the filesystem it can reach.
const WORKSPACE_ROOT: &str = "/";

/// The environment of a code interpreter session, as the shared shell asks it for a command
/// neither of its own catalogs names.
#[derive(Debug, Default)]
pub struct SandboxPrograms;

impl EnvironmentPrograms for SandboxPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        match name {
            "ls" => Some(list_entries(arguments, files)),
            "head" => Some(read_lines(name, arguments, files, true)),
            "tail" => Some(read_lines(name, arguments, files, false)),
            "wc" => Some(count(arguments, files)),
            "pwd" => Some(ProgramOutcome {
                status: 0,
                output: vec![WORKSPACE_ROOT.to_string()],
            }),
            // A program this environment does not have: the shell reports it as a command
            // it could not run.
            _ => None,
        }
    }
}

/// The OPERANDS of a call — every argument that is not an option — and the count a `-n`
/// option names, which is what `head` and `tail` take.
///
/// The value of a `-n` written apart from its option (`-n 1`) is the option's, not an
/// operand: a utility that reads it as a path would report on a file nothing named.
fn call_of(arguments: &[String]) -> (Option<usize>, Vec<&str>) {
    let mut count: Option<usize> = None;
    let mut operands: Vec<&str> = Vec::new();
    let mut named = arguments.iter();
    while let Some(argument) = named.next() {
        if let Some(digits) = argument.strip_prefix("-n") {
            count = if digits.is_empty() {
                named.next().and_then(|value| value.parse().ok())
            } else {
                digits.parse().ok()
            };
            continue;
        }
        if argument.starts_with('-') {
            continue;
        }
        operands.push(argument.as_str());
    }
    (count, operands)
}

/// The operands of a call: every argument that is not an option or an option's value.
fn operands(arguments: &[String]) -> Vec<&str> {
    call_of(arguments).1
}

/// `ls`: the names the directory holds, one per line, or the path itself when it names a
/// file — and the utility's own diagnostic for a path the filesystem holds nothing at.
///
/// With no operand it lists the directory the program runs in, which is the workspace root.
fn list_entries(arguments: &[String], files: &mut dyn ProgramFiles) -> ProgramOutcome {
    let named = operands(arguments);
    let paths: Vec<&str> = if named.is_empty() {
        vec![WORKSPACE_ROOT]
    } else {
        named
    };
    let mut output: Vec<String> = Vec::new();
    let mut status = 0;
    for path in paths {
        if files.read(path).is_some() {
            // A file is listed as the path that was named, which is what `ls` answers for
            // one.
            output.push(path.to_string());
            continue;
        }
        let entries = files.entries_in(path);
        if entries.is_empty() {
            output.push(format!(
                "ls: cannot access '{path}': No such file or directory"
            ));
            status = CANNOT_ACCESS;
            continue;
        }
        output.extend(entries);
    }
    ProgramOutcome { status, output }
}

/// `head` / `tail`: the first (or last) lines of each file named.
fn read_lines(
    name: &str,
    arguments: &[String],
    files: &mut dyn ProgramFiles,
    from_the_start: bool,
) -> ProgramOutcome {
    let (named, paths) = call_of(arguments);
    let wanted = named.unwrap_or(DEFAULT_LINE_COUNT);
    let mut output: Vec<String> = Vec::new();
    let mut status = 0;
    for path in paths {
        let Some(contents) = files.read(path) else {
            output.push(format!(
                "{name}: cannot open '{path}' for reading: No such file or directory"
            ));
            status = CANNOT_ACCESS;
            continue;
        };
        let lines: Vec<&str> = contents.strip_suffix('\n').unwrap_or(&contents).lines().collect();
        let taken: Vec<&str> = if from_the_start {
            lines.iter().take(wanted).copied().collect()
        } else {
            lines
                .iter()
                .skip(lines.len().saturating_sub(wanted))
                .copied()
                .collect()
        };
        output.extend(taken.into_iter().map(str::to_string));
    }
    ProgramOutcome { status, output }
}

/// `wc`: the lines, words and bytes of each file named, as the utility writes them — and
/// only the one a `-l`, `-w` or `-c` option asks for.
fn count(arguments: &[String], files: &mut dyn ProgramFiles) -> ProgramOutcome {
    let lines_only = arguments.iter().any(|argument| argument == "-l");
    let words_only = arguments.iter().any(|argument| argument == "-w");
    let bytes_only = arguments.iter().any(|argument| argument == "-c");
    let mut output: Vec<String> = Vec::new();
    let mut status = 0;
    for path in operands(arguments) {
        let Some(contents) = files.read(path) else {
            output.push(format!(
                "wc: {path}: No such file or directory"
            ));
            status = CANNOT_ACCESS;
            continue;
        };
        let lines = contents.lines().count();
        let words = contents.split_whitespace().count();
        let bytes = contents.len();
        let counted = match (lines_only, words_only, bytes_only) {
            (true, false, false) => format!("{lines} {path}"),
            (false, true, false) => format!("{words} {path}"),
            (false, false, true) => format!("{bytes} {path}"),
            _ => format!("{lines} {words} {bytes} {path}"),
        };
        output.push(counted);
    }
    ProgramOutcome { status, output }
}
