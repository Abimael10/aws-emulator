// SPDX-License-Identifier: CC0-1.0

//! The FILES a bounded shell session works over, and the utilities that work over them.
//!
//! # Why this is one module
//!
//! Two kinds of session in this workspace perform caller-supplied programs: the synchronous
//! one every service with a caller-supplied script drives
//! ([`crate::posix_shell_session`]) and the PROCESS one a container's program runs in
//! (`service-com-amazonaws-ecs-amazonec2containerservicev20141113`'s
//! `state::container_shell`, which loops, waits, installs signal handlers and delivers each
//! line as it is written). What differs between them is the WALK over the grammar's tree —
//! one is driven synchronously by callers that perform a program while holding their own
//! store's lock, the other awaits — and what does NOT differ is everything here:
//!
//! * the SEAM a session reaches its files through ([`ShellFiles`]), which is what lets one
//!   session work over a machine's tree and another over a container's own layer with the
//!   volumes mounted into it;
//! * the file UTILITIES ([`FILE_COMMANDS`], [`perform_file_command`]) — `cat`, `ls`,
//!   `mkdir`, `touch`, `rm` and `sync` — each with the output form and the diagnostic
//!   wording the utility itself is specified to write;
//! * the `test` questions that are about a FILE ([`file_test_answer`]), beside the ones about
//!   values alone ([`crate::posix_shell_session::value_test_answer`]);
//! * the REDIRECTIONS of the three standard streams ([`Redirections`],
//!   [`AppliedRedirect::of`]), read off the grammar's own redirection node.
//!
//! So each session plumbs its own streams and spends its own budget, and the answer to "what
//! does `ls -l` write", "what does `rm` say about a directory" and "which descriptor does
//! `2>&1` duplicate" is written once, here.
//!
//! # What it does not do
//!
//! It opens, resolves and lists nothing of the host: every answer comes from the
//! [`ShellFiles`] a session was handed, which is memory of this process. It holds no state
//! and no bounds of its own — the holder of the files bounds what they may hold, and the
//! session bounds the commands.

use std::time::SystemTime;

use super::posix_shell_volume::EntryKind;

/// Why a file operation did not happen: the kernel's own error, whose TEXT is what a
/// program's diagnostic about it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRefusal {
    /// `ENOENT`: nothing is at that path, or a directory on the way to it is missing.
    NotFound,
    /// `EISDIR`: the path names a directory, which is not something to read or write bytes
    /// in.
    IsADirectory,
    /// `ENOTDIR`: something on the way to the path is a file rather than a directory.
    NotADirectory,
    /// `EEXIST`: something is already there.
    AlreadyExists,
    /// `EROFS`: the filesystem the path is on is mounted read-only.
    ReadOnly,
    /// `ENOSPC`: the filesystem will hold no more.
    NoSpace,
}

impl FileRefusal {
    /// The text this error is reported with, which is what a program's diagnostic says.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NotFound => "No such file or directory",
            Self::IsADirectory => "Is a directory",
            Self::NotADirectory => "Not a directory",
            Self::AlreadyExists => "File exists",
            Self::ReadOnly => "Read-only file system",
            Self::NoSpace => "No space left on device",
        }
    }
}

/// One entry of the files as a LONG listing reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// What the entry is.
    pub kind: EntryKind,
    /// How many bytes it holds.
    pub size: usize,
    /// When it was last written.
    pub modified: SystemTime,
    /// The owning user, as a listing names it.
    pub user: String,
    /// The owning group, as a listing names it.
    pub group: String,
}

/// The FILES one session's commands read and write: the seam between a session and whatever
/// holds its files — a machine's tree, a session's own working tree, or a container's own
/// layer with the volumes mounted into it.
///
/// Every path arrives as the program wrote it; resolving one is the holder's, because only
/// the holder knows what a relative path is relative to and which store a path lands in.
pub trait ShellFiles {
    /// What reading `path` answers, or the error the read fails with.
    fn read(&self, path: &str) -> Result<String, FileRefusal>;

    /// Write `text` at `path`, replacing what is there or ADDING to it.
    fn write(&mut self, path: &str, text: &str, append: bool) -> Result<(), FileRefusal>;

    /// Make `path` a directory, with every directory above it.
    fn make_directories(&mut self, path: &str) -> Result<(), FileRefusal>;

    /// Remove the file at `path`.
    fn remove_file(&mut self, path: &str) -> Result<(), FileRefusal>;

    /// Whether anything is at `path` at all.
    fn exists(&self, path: &str) -> bool;

    /// Whether `path` is a regular file.
    fn is_file(&self, path: &str) -> bool;

    /// Whether `path` is a directory.
    fn is_directory(&self, path: &str) -> bool;

    /// Whether a write to `path` would be performed at all, which is what `test -w` asks.
    fn writable(&self, path: &str) -> bool;

    /// The names of the entries directly under the directory `path`, sorted.
    fn entry_names(&self, path: &str) -> Vec<String>;

    /// What a LONG listing reports about `path` — and `None` for a holder that keeps no such
    /// facts, which is what makes `ls -l` a form that session does not perform.
    fn entry(&self, _path: &str) -> Option<FileEntry> {
        None
    }

    /// Whether this holder keeps DIRECTORIES of its own. A holder that does not — one where a
    /// path is a key and a directory is merely a path some file lies under — has every
    /// directory above a file already, which is what a `mkdir` without `-p` asks about.
    fn keeps_directories(&self) -> bool {
        true
    }
}

/// The utilities this module performs over a session's files, as the names a command line
/// invokes them by.
///
/// Each one's whole effect is on those files and on what the command writes: `cat` reads
/// files (or its standard input), `ls` names what the files hold, `mkdir`, `touch` and `rm`
/// change them, and `sync` has nothing to flush, because a write to files held in memory is
/// in them the instant the write is performed.
pub const FILE_COMMANDS: &[&str] = &["cat", "ls", "mkdir", "touch", "rm", "sync"];

/// What performing one of [`FILE_COMMANDS`] produced: the status it answers and the bytes it
/// wrote to each of its streams, which the session then plumbs through its own redirections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileCommandOutcome {
    /// Whether the utility succeeded — "exit with a value >0 if an error occurred".
    pub succeeded: bool,
    /// What it wrote to standard output.
    pub output: String,
    /// What it wrote to standard error.
    pub errors: String,
}

impl FileCommandOutcome {
    /// An outcome that has written nothing yet.
    fn succeeding() -> Self {
        Self {
            succeeded: true,
            ..Self::default()
        }
    }

    /// Record `text` as written to standard error, and the utility as failed.
    fn failed_with(&mut self, text: &str) {
        self.errors.push_str(text);
        self.succeeded = false;
    }
}

/// Perform the utility `name` with `arguments` over `files`, with `input` as what its
/// standard input holds — `None` where the session has none to give it.
///
/// `None` is a utility, option or operand form this module does not perform, which is the
/// fail-closed answer a session reports as a command it could not run: `cat` with no operand
/// and no input (there is no terminal behind it), `ls` with an option beyond `-1` and `-l`,
/// and `ls -l` over files that keep no facts to report.
#[must_use]
pub fn perform_file_command(
    name: &str,
    arguments: &[String],
    files: &mut dyn ShellFiles,
    input: Option<&str>,
) -> Option<FileCommandOutcome> {
    match name {
        "cat" => concatenate(arguments, files, input),
        "ls" => list(arguments, files),
        "mkdir" => Some(make_directories(arguments, files)),
        "touch" => Some(touch(arguments, files)),
        "rm" => Some(remove(arguments, files)),
        // "sync() causes all pending modifications to filesystem metadata and cached file
        // data to be written": a write to these files is IN them the instant the write is
        // performed, so there is nothing pending and the utility writes nothing.
        "sync" => Some(FileCommandOutcome::succeeding()),
        _ => None,
    }
}

/// `cat [FILE...]`: the contents of each file named, in order, on standard output.
///
/// "cat … shall exit with a value >0 if an error occurred", and a path the files will not
/// read is one such error: it is reported on standard error in the utility's own wording and
/// the command answers failure, without stopping the files after it. With no file named it
/// reads STANDARD INPUT.
fn concatenate(
    arguments: &[String],
    files: &dyn ShellFiles,
    input: Option<&str>,
) -> Option<FileCommandOutcome> {
    let paths = operands_of(arguments);
    if paths.is_empty() {
        let input = input?;
        return Some(FileCommandOutcome {
            succeeded: true,
            output: input.to_string(),
            errors: String::new(),
        });
    }
    let mut outcome = FileCommandOutcome::succeeding();
    for path in paths {
        match files.read(&path) {
            Ok(contents) => outcome.output.push_str(&contents),
            Err(refusal) => {
                outcome.failed_with(&format!("cat: {path}: {}\n", refusal.message()));
            }
        }
    }
    Some(outcome)
}

/// `ls [-1] [-l] [PATH...]`: what the files hold at the paths the command was given.
///
/// "For each operand that names a file of a type other than directory … `ls` shall write the
/// name of the file"; for one that names a directory it writes "the names of files contained
/// within" it, and "if no operands are given, the contents of the current directory shall be
/// written". The names are sorted, which is the order this utility is specified to write them
/// in, and `-1` asks for the one-name-per-line form it writes anyway.
///
/// `-l` writes "the long format" ([`long_listing`]), which a holder that keeps no facts about
/// its entries cannot answer — that is then a form the session does not perform, rather than
/// a listing with invented modes and times in it. Every other option is a form this does not
/// perform either.
fn list(arguments: &[String], files: &dyn ShellFiles) -> Option<FileCommandOutcome> {
    let mut long = false;
    let mut operands: Vec<String> = Vec::new();
    for argument in arguments {
        match argument.as_str() {
            "-l" => long = true,
            "-1" => {}
            option if option.len() > 1 && option.starts_with('-') => return None,
            operand => operands.push(operand.to_string()),
        }
    }
    operands.sort();
    if operands.is_empty() {
        operands.push(CURRENT_DIRECTORY.to_string());
    }
    let mut outcome = FileCommandOutcome::succeeding();
    let mut blocks: Vec<String> = Vec::new();
    // A file operand is written as the caller spelled it, and all of them together make the
    // first block — the order `ls` writes a mixed list of operands in.
    let mut named: Vec<(String, String)> = Vec::new();
    let mut directories: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for path in &operands {
        if files.is_directory(path) {
            let entries = files
                .entry_names(path)
                .into_iter()
                .map(|name| (entry_path(path, &name), name))
                .collect();
            directories.push((path.clone(), entries));
        } else if files.exists(path) {
            named.push((path.clone(), path.clone()));
        } else {
            outcome.failed_with(&format!(
                "ls: cannot access '{path}': {}\n",
                FileRefusal::NotFound.message()
            ));
        }
    }
    if !named.is_empty() {
        blocks.push(listed(&named, long, files)?);
    }
    let headed = operands.len() > 1;
    for (path, entries) in directories {
        let block = listed(&entries, long, files)?;
        if headed {
            blocks.push(format!("{path}:\n{block}"));
        } else {
            blocks.push(block);
        }
    }
    // Each block already ends in a newline, so joining them with one leaves the blank line
    // `ls` separates two blocks with.
    outcome.output = blocks.join("\n");
    Some(outcome)
}

/// One block of an `ls` listing: `entries` as `(path, name)` pairs, one line each — the name
/// alone, or the long format when `long`.
///
/// The long format's size column is padded to the widest size in the BLOCK, which is how the
/// utility lines its columns up over the entries it is writing. `None` where a long listing
/// was asked for over an entry whose facts the files do not keep.
fn listed(
    entries: &[(String, String)],
    long: bool,
    files: &dyn ShellFiles,
) -> Option<String> {
    if !long {
        return Some(
            entries
                .iter()
                .map(|(_, name)| format!("{name}\n"))
                .collect(),
        );
    }
    let mut reported: Vec<(String, FileEntry, String)> = Vec::new();
    for (path, name) in entries {
        let entry = files.entry(path)?;
        reported.push((path.clone(), entry, name.clone()));
    }
    let width = reported
        .iter()
        .map(|(_, entry, _)| entry.size.to_string().len())
        .max()
        .unwrap_or_default();
    Some(
        reported
            .iter()
            .map(|(path, entry, name)| long_listing(path, entry, name, width, files))
            .collect(),
    )
}

/// ONE line of the long listing of the entry at `path`, written under `name`, in the fields
/// `ls -l` writes: the entry's type and permissions, its link count, its owner and its group,
/// its size padded to `width`, when it was last written and its name.
///
/// The permissions are the ones such an entry is created with: a file is opened for creation
/// with mode `666` and a directory made with `777`, which a process's own file-creation mask
/// — `022` for the processes these sessions stand for — leaves at `644` and `755`. The link
/// count of a directory is itself, its parent's entry for it and one per directory under it;
/// everything else has the one link its name is.
fn long_listing(
    path: &str,
    entry: &FileEntry,
    name: &str,
    width: usize,
    files: &dyn ShellFiles,
) -> String {
    let (kind, permissions) = match entry.kind {
        EntryKind::File => ('-', FILE_PERMISSIONS),
        EntryKind::Directory => ('d', DIRECTORY_PERMISSIONS),
        EntryKind::Device => ('c', DEVICE_PERMISSIONS),
    };
    let links = match entry.kind {
        EntryKind::Directory => files
            .entry_names(path)
            .iter()
            .filter(|held| files.is_directory(&entry_path(path, held)))
            .count()
            .saturating_add(2),
        _ => 1,
    };
    let size = entry.size;
    format!(
        "{kind}{permissions} {links} {} {} {size:>width$} {} {name}\n",
        entry.user,
        entry.group,
        listing_time(entry.modified),
    )
}

/// `mkdir [-p] PATH...`: "the `mkdir` utility shall create the directories specified by the
/// operands", and `-p` also creates "any missing intermediate pathname components" — and
/// makes an existing directory no error at all ("do not treat an existing directory as an
/// error").
fn make_directories(arguments: &[String], files: &mut dyn ShellFiles) -> FileCommandOutcome {
    let parents = arguments.iter().any(|argument| argument == "-p");
    let mut outcome = FileCommandOutcome::succeeding();
    for path in operands_of(arguments) {
        if files.is_directory(&path) {
            if !parents {
                outcome.failed_with(&format!(
                    "mkdir: cannot create directory '{path}': {}\n",
                    FileRefusal::AlreadyExists.message()
                ));
            }
            continue;
        }
        // Without `-p` ONE directory is made, so the one above it has to be there already.
        // Files that keep no directories of their own have every path above one already.
        let missing_parent = !parents
            && files.keeps_directories()
            && parent_of(&path).is_some_and(|parent| !files.is_directory(&parent));
        let refusal = if missing_parent {
            Some(FileRefusal::NotFound)
        } else {
            files.make_directories(&path).err()
        };
        if let Some(refusal) = refusal {
            outcome.failed_with(&format!(
                "mkdir: cannot create directory '{path}': {}\n",
                refusal.message()
            ));
        }
    }
    outcome
}

/// `touch PATH...`: "the `touch` utility shall change the last data modification timestamps …
/// of one or more files", creating one the files hold nothing at.
fn touch(arguments: &[String], files: &mut dyn ShellFiles) -> FileCommandOutcome {
    let mut outcome = FileCommandOutcome::succeeding();
    for path in operands_of(arguments) {
        if files.is_directory(&path) {
            // A directory's own timestamp is not what this utility is given a path for.
            continue;
        }
        // Files that keep no modification time have nothing to change about a path they
        // already hold; where they do, adding nothing to it is what moves that time.
        if files.exists(&path) && files.entry(&path).is_none() {
            continue;
        }
        if let Err(refusal) = files.write(&path, "", true) {
            outcome.failed_with(&format!(
                "touch: cannot touch '{path}': {}\n",
                refusal.message()
            ));
        }
    }
    outcome
}

/// `rm [-f] PATH...`: the files named removed.
///
/// `-f` passes over a path there is nothing at ("do not write diagnostic messages … do not
/// modify the exit status"), and a directory is not removed by this utility at all.
fn remove(arguments: &[String], files: &mut dyn ShellFiles) -> FileCommandOutcome {
    let forced = arguments
        .iter()
        .any(|argument| argument.starts_with('-') && argument.contains('f'));
    let mut outcome = FileCommandOutcome::succeeding();
    for path in operands_of(arguments) {
        match files.remove_file(&path) {
            Ok(()) => {}
            Err(FileRefusal::NotFound) if forced => {}
            Err(refusal) => {
                outcome.failed_with(&format!(
                    "rm: cannot remove '{path}': {}\n",
                    refusal.message()
                ));
            }
        }
    }
    outcome
}

/// What the `test` questions about a FILE answer for `arguments` over `files`, and `None` for
/// an expression that asks about something else — which is then read as a question about
/// values alone ([`crate::posix_shell_session::value_test_answer`]).
///
/// The primaries a program asks about storage: whether the files hold a path (`-e`), a
/// regular file (`-f`), a directory (`-d`), a file with something in it (`-s`), one that can
/// be read (`-r`) and one that can be written (`-w`) — which is what a read-only mount
/// answers false for.
#[must_use]
pub fn file_test_answer(arguments: &[String], files: &dyn ShellFiles) -> Option<bool> {
    let [primary, path] = arguments else {
        return None;
    };
    match primary.as_str() {
        "-e" => Some(files.exists(path)),
        "-f" => Some(files.is_file(path)),
        "-d" => Some(files.is_directory(path)),
        // "-s file: True if file exists and has a size greater than zero."
        "-s" => Some(files.read(path).is_ok_and(|held| !held.is_empty())),
        "-r" => Some(files.exists(path)),
        "-w" => Some(files.writable(path)),
        _ => None,
    }
}

/// Where one command's standard output, standard error and standard input were REDIRECTED to,
/// for the command being performed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redirections {
    /// `> path` / `>> path`: the file standard output goes to, and whether the write ADDS to
    /// what the file holds.
    pub out: Option<(String, bool)>,
    /// `2> path` / `2>> path`, read the same way.
    pub error: Option<(String, bool)>,
    /// `< path`: the file standard input is read from.
    pub input: Option<String>,
    /// The TEXT standard input carries, for a command whose input is written inside the
    /// command itself: a here-document (`<<WORD`, `<<-WORD`) or a here-string (`<<<WORD`). It
    /// is the input itself rather than a path, so nothing of the files is read for it.
    pub input_text: Option<String>,
    /// `2>&1`: this command's diagnostics go where its output goes.
    pub error_to_out: bool,
    /// `1>&2`: this command's output goes where its diagnostics go.
    pub out_to_error: bool,
}

/// ONE redirection of a command, as a session records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedRedirect {
    /// `> path` / `>> path`, and whether the write ADDS to what the file holds.
    Out(String, bool),
    /// `2> path` / `2>> path`, read the same way.
    Error(String, bool),
    /// `< path`.
    Input(String),
    /// `2>&1`.
    ErrorToOut,
    /// `1>&2`.
    OutToError,
}

/// The descriptor a command's own writes go to.
pub const STANDARD_OUTPUT: i32 = 1;

/// The descriptor a command reports what it could not do on.
pub const STANDARD_ERROR: i32 = 2;

/// The descriptor a command reads from.
pub const STANDARD_INPUT: i32 = 0;

impl AppliedRedirect {
    /// The redirection the grammar's node describes, with its target word already EXPANDED by
    /// the session (which is the only thing that knows how to expand one) — and `None` for a
    /// form no session performs.
    ///
    /// The forms read here are the ones a program written against storage uses: `> path` and
    /// `>| path` write standard output from the start of the file, `>> path` adds to what it
    /// holds ("the file shall be opened for appending"), `2>`/`2>>` do the same for standard
    /// error, `< path` reads standard input from a file, and `2>&1`/`1>&2` duplicate one
    /// standard stream onto the other — which is how a program asks for its diagnostics on
    /// the one stream its caller reads. A read-write open, a here-document, a here-string, a
    /// duplication of any other descriptor and a descriptor other than the three standard
    /// ones are forms no session performs.
    #[must_use]
    pub fn of(
        descriptor: Option<i32>,
        kind: &::brush_parser::ast::IoFileRedirectKind,
        expanded_target: &str,
    ) -> Option<Self> {
        use ::brush_parser::ast::IoFileRedirectKind as Kind;
        match kind {
            Kind::DuplicateOutput => {
                let duplicated: i32 = expanded_target.parse().ok()?;
                match (descriptor.unwrap_or(STANDARD_OUTPUT), duplicated) {
                    (STANDARD_ERROR, STANDARD_OUTPUT) => Some(Self::ErrorToOut),
                    (STANDARD_OUTPUT, STANDARD_ERROR) => Some(Self::OutToError),
                    _ => None,
                }
            }
            Kind::Read => (descriptor.unwrap_or(STANDARD_INPUT) == STANDARD_INPUT)
                .then(|| Self::Input(expanded_target.to_string())),
            Kind::Write | Kind::Clobber | Kind::Append => {
                let append = matches!(kind, Kind::Append);
                match descriptor.unwrap_or(STANDARD_OUTPUT) {
                    STANDARD_OUTPUT => Some(Self::Out(expanded_target.to_string(), append)),
                    STANDARD_ERROR => Some(Self::Error(expanded_target.to_string(), append)),
                    _ => None,
                }
            }
            Kind::ReadAndWrite | Kind::DuplicateInput => None,
        }
    }

    /// Record this redirection among the ones the command being performed carries. A later
    /// redirection of the same descriptor replaces an earlier one, as the shell's last open of
    /// a descriptor is the one that stands.
    pub fn record(self, redirections: &mut Redirections) {
        match self {
            Self::Out(path, append) => redirections.out = Some((path, append)),
            Self::Error(path, append) => redirections.error = Some((path, append)),
            Self::Input(path) => redirections.input = Some(path),
            Self::ErrorToOut => redirections.error_to_out = true,
            Self::OutToError => redirections.out_to_error = true,
        }
    }
}

/// The directory an `ls` with no operand lists, which is the directory the session's own
/// relative paths are resolved against.
const CURRENT_DIRECTORY: &str = ".";

/// The permissions a long listing reports for a FILE: `666` less a `022` creation mask.
const FILE_PERMISSIONS: &str = "rw-r--r--";

/// The permissions a long listing reports for a DIRECTORY: `777` less that same mask.
const DIRECTORY_PERMISSIONS: &str = "rwxr-xr-x";

/// The permissions a long listing reports for a DEVICE, which is the `666` a null device
/// carries.
const DEVICE_PERMISSIONS: &str = "rw-rw-rw-";

/// The format a long listing writes an entry's modification time in: `ls -l` writes "the
/// month, day and hour" for a time inside the last six months, which is every file one of
/// these sessions wrote.
const LISTING_TIME_FORMAT: &str = "%b %e %H:%M";

/// `at` as a long listing writes it ([`LISTING_TIME_FORMAT`]), in UTC — the time zone the
/// processes these sessions stand for are in unless their image says otherwise.
///
/// The calendar conversion comes from the maintained calendar crate's FALLIBLE reading of an
/// instant, so a time outside the dates a calendar can state leaves the field empty rather
/// than failing the listing.
fn listing_time(at: SystemTime) -> String {
    let Ok(since) = at.duration_since(SystemTime::UNIX_EPOCH) else {
        return String::new();
    };
    let Ok(seconds) = i64::try_from(since.as_secs()) else {
        return String::new();
    };
    ::chrono::DateTime::from_timestamp(seconds, 0)
        .map(|at| at.format(LISTING_TIME_FORMAT).to_string())
        .unwrap_or_default()
}

/// The path of the entry `name` under the directory `path`.
fn entry_path(path: &str, name: &str) -> String {
    format!("{}/{name}", path.trim_end_matches('/'))
}

/// The OPERANDS of a command line: the arguments that are not options. A lone `-` is an
/// operand, as it is for every utility that reads one.
#[must_use]
pub fn operands_of(arguments: &[String]) -> Vec<String> {
    arguments
        .iter()
        .filter(|argument| argument.len() == 1 || !argument.starts_with('-'))
        .cloned()
        .collect()
}

/// Whether a command line names an OPERAND at all — what a session asks before it resolves
/// the standard input a utility with none reads instead.
#[must_use]
pub fn names_an_operand(arguments: &[String]) -> bool {
    !operands_of(arguments).is_empty()
}

/// The directory the path `path` lies in, as a command that needs it there already reads it —
/// and `None` for a path with no directory above it.
fn parent_of(path: &str) -> Option<String> {
    super::posix_shell_volume::parent_path(path)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    use super::{
        file_test_answer, perform_file_command, AppliedRedirect, FileEntry, FileRefusal,
        ShellFiles,
    };
    use crate::posix_shell_volume::EntryKind;

    /// The simplest holder of files there is: a map of path to contents, with directories of
    /// its own and one path it will not take a write to — which is what a read-only mount and
    /// a sealed file both look like from a utility's side.
    #[derive(Default)]
    struct Held {
        files: BTreeMap<String, String>,
        directories: Vec<String>,
        read_only: Vec<String>,
        facts: bool,
    }

    impl ShellFiles for Held {
        fn read(&self, path: &str) -> Result<String, FileRefusal> {
            if self.is_directory(path) {
                return Err(FileRefusal::IsADirectory);
            }
            self.files
                .get(path)
                .cloned()
                .ok_or(FileRefusal::NotFound)
        }

        fn write(&mut self, path: &str, text: &str, append: bool) -> Result<(), FileRefusal> {
            if self.read_only.iter().any(|held| held == path) {
                return Err(FileRefusal::ReadOnly);
            }
            let held = self.files.entry(path.to_string()).or_default();
            if !append {
                held.clear();
            }
            held.push_str(text);
            Ok(())
        }

        fn make_directories(&mut self, path: &str) -> Result<(), FileRefusal> {
            if self.read_only.iter().any(|held| held == path) {
                return Err(FileRefusal::ReadOnly);
            }
            self.directories.push(path.to_string());
            Ok(())
        }

        fn remove_file(&mut self, path: &str) -> Result<(), FileRefusal> {
            if self.read_only.iter().any(|held| held == path) {
                return Err(FileRefusal::ReadOnly);
            }
            self.files
                .remove(path)
                .map(|_| ())
                .ok_or(FileRefusal::NotFound)
        }

        fn exists(&self, path: &str) -> bool {
            self.files.contains_key(path) || self.is_directory(path)
        }

        fn is_file(&self, path: &str) -> bool {
            self.files.contains_key(path)
        }

        fn is_directory(&self, path: &str) -> bool {
            self.directories.iter().any(|held| held == path)
        }

        fn writable(&self, path: &str) -> bool {
            !self.read_only.iter().any(|held| held == path)
        }

        fn entry_names(&self, path: &str) -> Vec<String> {
            let under = format!("{}/", path.trim_end_matches('/'));
            let mut names: Vec<String> = self
                .files
                .keys()
                .filter_map(|held| held.strip_prefix(under.as_str()))
                .map(|name| name.split('/').next().unwrap_or(name).to_string())
                .collect();
            names.sort();
            names.dedup();
            names
        }

        fn entry(&self, path: &str) -> Option<FileEntry> {
            if !self.facts {
                return None;
            }
            let contents = self.files.get(path)?;
            Some(FileEntry {
                kind: EntryKind::File,
                size: contents.len(),
                modified: SystemTime::UNIX_EPOCH,
                user: "root".to_string(),
                group: "root".to_string(),
            })
        }
    }

    /// RULE: each utility answers out of the files it was handed, in its own output form, and
    /// reports what it could not do in its own wording — with the kernel's own error text in
    /// it, so a write a read-only mount refuses says `Read-only file system`.
    #[test]
    fn each_utility_answers_out_of_the_files_it_was_handed() {
        let mut held = Held {
            directories: vec!["/data".to_string()],
            read_only: vec!["/data/sealed".to_string()],
            facts: true,
            ..Held::default()
        };
        held.files
            .insert("/data/payload".to_string(), "abc\n".to_string());
        held.files
            .insert("/data/sealed".to_string(), "kept\n".to_string());

        // One row: the command, its arguments, what it writes to standard output, whether it
        // succeeded, and what its diagnostic must carry.
        struct Row {
            case: &'static str,
            name: &'static str,
            arguments: &'static [&'static str],
            output: &'static str,
            succeeded: bool,
            diagnostic: Option<&'static str>,
        }
        let rows = [
            Row {
                case: "a file read",
                name: "cat",
                arguments: &["/data/payload"],
                output: "abc\n",
                succeeded: true,
                diagnostic: None,
            },
            Row {
                case: "a file the holder has none of",
                name: "cat",
                arguments: &["/data/absent"],
                output: "",
                succeeded: false,
                diagnostic: Some("cat: /data/absent: No such file or directory"),
            },
            Row {
                case: "a listing of a directory",
                name: "ls",
                arguments: &["/data"],
                output: "payload\nsealed\n",
                succeeded: true,
                diagnostic: None,
            },
            Row {
                case: "a long listing of a file",
                name: "ls",
                arguments: &["-l", "/data/payload"],
                output: "-rw-r--r-- 1 root root 4 Jan  1 00:00 /data/payload\n",
                succeeded: true,
                diagnostic: None,
            },
            Row {
                case: "a write the holder refuses",
                name: "touch",
                arguments: &["/data/sealed"],
                output: "",
                succeeded: false,
                diagnostic: Some("touch: cannot touch '/data/sealed': Read-only file system"),
            },
            Row {
                case: "a removal the holder refuses",
                name: "rm",
                arguments: &["/data/sealed"],
                output: "",
                succeeded: false,
                diagnostic: Some("rm: cannot remove '/data/sealed': Read-only file system"),
            },
            Row {
                case: "a removal of a path there is nothing at, forced",
                name: "rm",
                arguments: &["-f", "/data/absent"],
                output: "",
                succeeded: true,
                diagnostic: None,
            },
            Row {
                case: "a flush of what is already written",
                name: "sync",
                arguments: &[],
                output: "",
                succeeded: true,
                diagnostic: None,
            },
        ];
        for row in rows {
            let case = row.case;
            let arguments: Vec<String> = row
                .arguments
                .iter()
                .map(|word| (*word).to_string())
                .collect();
            let outcome = perform_file_command(row.name, &arguments, &mut held, None)
                .unwrap_or_else(|| panic!("case {case:?}: expected the utility to be performed"));
            assert_eq!(
                outcome.output, row.output,
                "case {case:?}: expected {:?} on standard output, got {:?}",
                row.output, outcome.output
            );
            assert_eq!(
                outcome.succeeded, row.succeeded,
                "case {case:?}: expected success to be {}, got {outcome:?}",
                row.succeeded
            );
            match row.diagnostic {
                Some(text) => assert!(
                    outcome.errors.contains(text),
                    "case {case:?}: expected a diagnostic carrying {text:?}, got {:?}",
                    outcome.errors
                ),
                None => assert!(
                    outcome.errors.is_empty(),
                    "case {case:?}: expected no diagnostic, got {:?}",
                    outcome.errors
                ),
            }
        }
    }

    /// RULE: a long listing is a form only files that KEEP those facts can answer, so a holder
    /// that keeps none reports the form as one it does not perform rather than inventing a
    /// mode or a time.
    #[test]
    fn a_long_listing_needs_facts_the_files_keep() {
        let mut held = Held::default();
        held.files.insert("/one".to_string(), "x".to_string());
        let long = perform_file_command("ls", &["-l".to_string(), "/one".to_string()], &mut held, None);
        assert!(
            long.is_none(),
            "expected a long listing over files that keep no facts not to be performed, got \
             {long:?}"
        );
        let plain = perform_file_command("ls", &["/one".to_string()], &mut held, None)
            .expect("a plain listing is performed");
        assert_eq!(plain.output, "/one\n", "got {plain:?}");
    }

    /// RULE: the `test` questions about a file are answered out of the files, and a question
    /// about something else is left to the reading of values.
    #[test]
    fn the_file_questions_are_answered_out_of_the_files() {
        let mut held = Held {
            directories: vec!["/data".to_string()],
            read_only: vec!["/data/sealed".to_string()],
            ..Held::default()
        };
        held.files.insert("/data/payload".to_string(), "abc".to_string());
        held.files.insert("/data/empty".to_string(), String::new());
        held.files.insert("/data/sealed".to_string(), "kept".to_string());

        // One row: the question the expression asks, and the answer.
        struct Row {
            case: &'static str,
            question: &'static [&'static str],
            expected: Option<bool>,
        }
        let rows = [
            Row {
                case: "a path it holds",
                question: &["-e", "/data/payload"],
                expected: Some(true),
            },
            Row {
                case: "a path it does not",
                question: &["-e", "/data/absent"],
                expected: Some(false),
            },
            Row {
                case: "a regular file",
                question: &["-f", "/data/payload"],
                expected: Some(true),
            },
            Row {
                case: "a directory",
                question: &["-d", "/data"],
                expected: Some(true),
            },
            Row {
                case: "a file with something in it",
                question: &["-s", "/data/payload"],
                expected: Some(true),
            },
            Row {
                case: "a file with nothing in it",
                question: &["-s", "/data/empty"],
                expected: Some(false),
            },
            Row {
                case: "a file it will not take a write to",
                question: &["-w", "/data/sealed"],
                expected: Some(false),
            },
            Row {
                case: "a question about values",
                question: &["-z", ""],
                expected: None,
            },
        ];
        for row in rows {
            let case = row.case;
            let question: Vec<String> = row
                .question
                .iter()
                .map(|word| (*word).to_string())
                .collect();
            let answered = file_test_answer(&question, &held);
            assert_eq!(
                answered, row.expected,
                "case {case:?}: expected {:?}, got {answered:?}",
                row.expected
            );
        }
    }

    /// RULE: a redirection is read off the grammar's node into the descriptor it opens, and a
    /// form no session performs is read as none.
    #[test]
    fn a_redirection_names_the_descriptor_it_opens() {
        use ::brush_parser::ast::IoFileRedirectKind as Kind;

        // (case, the descriptor the program named, the kind, the expanded target, what it is)
        let rows = [
            (
                "standard output from the start of the file",
                None,
                Kind::Write,
                "/data/out",
                Some(AppliedRedirect::Out("/data/out".to_string(), false)),
            ),
            (
                "standard output added to the file",
                None,
                Kind::Append,
                "/data/out",
                Some(AppliedRedirect::Out("/data/out".to_string(), true)),
            ),
            (
                "standard error to a file",
                Some(2),
                Kind::Write,
                "/data/err",
                Some(AppliedRedirect::Error("/data/err".to_string(), false)),
            ),
            (
                "standard input from a file",
                None,
                Kind::Read,
                "/data/in",
                Some(AppliedRedirect::Input("/data/in".to_string())),
            ),
            (
                "diagnostics onto the output stream",
                Some(2),
                Kind::DuplicateOutput,
                "1",
                Some(AppliedRedirect::ErrorToOut),
            ),
            (
                "output onto the diagnostics stream",
                Some(1),
                Kind::DuplicateOutput,
                "2",
                Some(AppliedRedirect::OutToError),
            ),
            (
                "a descriptor beyond the three standard ones",
                Some(3),
                Kind::Write,
                "/data/out",
                None,
            ),
            (
                "a duplication of another descriptor",
                Some(2),
                Kind::DuplicateOutput,
                "3",
                None,
            ),
            (
                "a read-write open",
                None,
                Kind::ReadAndWrite,
                "/data/both",
                None,
            ),
        ];
        for (case, descriptor, kind, target, expected) in rows {
            let applied = AppliedRedirect::of(descriptor, &kind, target);
            assert_eq!(
                applied, expected,
                "case {case:?}: expected {expected:?}, got {applied:?}"
            );
        }
    }
}
