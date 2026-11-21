// SPDX-License-Identifier: CC0-1.0

//! The PROGRAMS the environment of a code interpreter session provides, beside the shell's
//! own utilities.
//!
//! A command line names two kinds of command: the shell's own utilities, which the shared
//! session performs (`service_common::posix_shell_session::BUILT_IN_COMMANDS`,
//! `VOLUME_COMMANDS` and the text filters of
//! `service_common::posix_shell_text_filters`), and the programs the environment it runs in
//! has installed — which ones those are is the environment's fact, not the shell's. A code
//! interpreter sandbox is an environment with a filesystem of its own, so the programs it
//! provides are the READ-ONLY utilities that report on that filesystem, and these are they.
//!
//! Every one of them computes its answer from the session's own tree, through the files the
//! shared session hands it (`ProgramFiles`), or from the standard input the shared session
//! hands it for a call that names no path: there is no host process to start, no host `PATH`
//! to resolve against and no host file to open here, and a path is a key of that one
//! in-memory tree. A name this module provides no program for, and an invocation of one it
//! performs nothing of, fall back to the shell's own catalogs — and a name neither holds is
//! answered as the shell answers a command it could not run, so nothing is invented for a
//! utility the environment does not have.

use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

/// The programs this environment provides, as their names.
///
/// Each one REPORTS on the session's filesystem and changes nothing: `ls` lists a directory,
/// `cat`'s neighbours `head`, `tail` and `wc` read part of a file or count what is in it, and
/// `pwd` names the directory the program runs in. The three that READ an input read the one
/// the call gives them — the files it names, or standard input when it names none — so each of
/// them is a stage a command line can pipe into.
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

    /// The utilities that READ their input compute over the one the call gives them: the
    /// files it names, and STANDARD INPUT — the stage before them in a pipeline, or the file
    /// one was redirected from — when it names none, which is the input the utility reads for
    /// a call with no path operand.
    ///
    /// An invocation with neither is one this environment performs nothing of: there is no
    /// terminal behind standard input here, so nothing was read and the shell reports a
    /// command that did not run rather than one that read nothing.
    fn run_with_input(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        if !matches!(name, "head" | "tail" | "wc") {
            return self.run(name, arguments, files);
        }
        let (wanted, named) = call_of(arguments);
        if !named.is_empty() {
            return self.run(name, arguments, files);
        }
        let read = standard_input?;
        let wanted = wanted.unwrap_or(DEFAULT_LINE_COUNT);
        let output = match name {
            "head" => taken_lines(read, wanted, true),
            "tail" => taken_lines(read, wanted, false),
            // The counts alone: the utility names no file for an input it read from
            // standard input.
            _ => vec![counts_of(arguments, read, None)],
        };
        Some(ProgramOutcome { status: 0, output })
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
        output.extend(taken_lines(&contents, wanted, from_the_start));
    }
    ProgramOutcome { status, output }
}

/// The first (or last) `wanted` lines of `text`, as the lines `head` and `tail` write.
fn taken_lines(text: &str, wanted: usize, from_the_start: bool) -> Vec<String> {
    let lines: Vec<&str> = text.strip_suffix('\n').unwrap_or(text).lines().collect();
    if from_the_start {
        return lines
            .iter()
            .take(wanted)
            .map(|line| (*line).to_string())
            .collect();
    }
    lines
        .iter()
        .skip(lines.len().saturating_sub(wanted))
        .map(|line| (*line).to_string())
        .collect()
}

/// `wc`: the lines, words and bytes of each file named, as the utility writes them — and
/// only the one a `-l`, `-w` or `-c` option asks for.
fn count(arguments: &[String], files: &mut dyn ProgramFiles) -> ProgramOutcome {
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
        output.push(counts_of(arguments, &contents, Some(path)));
    }
    ProgramOutcome { status, output }
}

/// The counts `wc` writes for `text`: the one a `-l`, `-w` or `-c` option asks for, or all
/// three, followed by the name of what was read when the call named a path — and by nothing
/// for an input read from standard input, which is the form the utility writes for one.
fn counts_of(arguments: &[String], text: &str, path: Option<&str>) -> String {
    let lines_only = arguments.iter().any(|argument| argument == "-l");
    let words_only = arguments.iter().any(|argument| argument == "-w");
    let bytes_only = arguments.iter().any(|argument| argument == "-c");
    let lines = text.lines().count();
    let words = text.split_whitespace().count();
    let bytes = text.len();
    let named = path.map(|path| format!(" {path}")).unwrap_or_default();
    match (lines_only, words_only, bytes_only) {
        (true, false, false) => format!("{lines}{named}"),
        (false, true, false) => format!("{words}{named}"),
        (false, false, true) => format!("{bytes}{named}"),
        _ => format!("{lines} {words} {bytes}{named}"),
    }
}
