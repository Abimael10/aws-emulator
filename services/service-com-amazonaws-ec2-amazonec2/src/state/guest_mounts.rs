// SPDX-License-Identifier: CC0-1.0

//! What one instance's GUEST sees of the filesystems MOUNTED into it, and the programs it
//! asks about them with.
//!
//! # Why a guest needs this
//!
//! A program that has just mounted a filesystem checks that it mounted the filesystem it
//! meant to: the documented way to confirm an Amazon EFS mount is to ask what is mounted at
//! the path (`findmnt -no FSTYPE <dir>`, which answers the mount's own type) and what
//! filesystem that path lies on (`stat -f -c %T <dir>`, which answers the type a statfs of it
//! reports). The two answers come from different places in the kernel and are deliberately
//! asked together: a local directory standing in for a network filesystem answers neither of
//! them the way the real mount does.
//!
//! So a guest that reports nothing about its own mounts cannot be used for the thing a mount
//! is made for, however faithfully the filesystem behind it serves reads and writes.
//!
//! # One mount table, two readers
//!
//! The mounts are the session's own: the tree a program of this guest runs over holds them
//! ([`::service_common::posix_shell_volume::ShellVolume::mount`]), put there by the program
//! that mounted the filesystem — the mount program of the service that owns it, since the
//! address, the export and the files are that service's facts and this one holds none of
//! them. The programs below only REPORT what that one table holds, so what a path reads and
//! writes and what these programs say about it cannot disagree.
//!
//! # No host, no second notion of a filesystem
//!
//! Both programs are programs OF THE GUEST, performed by the same bounded, in-process shell
//! session the guest's own boot script runs in ([`super::guest_console`]): they open no file
//! of the host, call no statfs and report only what the session's mount table holds. What
//! each one answers for a path that is not on a mounted filesystem is nothing — this emulator
//! holds no filesystem facts for the machine's own storage — and an invocation asking for a
//! column or a format this emulator does not derive answers as a command that could not be
//! run, rather than a value nothing stands behind.

use ::service_common::posix_shell_session::{
    program_option_value, EnvironmentPrograms, ProgramFiles, ProgramOutcome,
};
use ::service_common::posix_shell_volume::MountFacts;

/// The program a guest asks what is mounted where with.
pub const FIND_MOUNT_PROGRAM: &str = "findmnt";

/// The program a guest asks the facts of a file — or, with `-f`, of the FILESYSTEM a path lies
/// on — with.
pub const STATUS_PROGRAM: &str = "stat";

/// The status a program of this guest exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status `findmnt` exits with when nothing it was asked about is mounted: "1 none of
/// specified devices/mountpoints found" (`findmnt(8)`, EXIT STATUS).
const NOTHING_MOUNTED: i32 = 1;

/// The status `stat` exits with when it could not report what it was asked about: "non-zero if
/// an error occurred" (`stat(1)`).
const CANNOT_STATUS: i32 = 1;

/// The columns `findmnt` writes when the invocation names none, in its own order: "TARGET
/// SOURCE FSTYPE OPTIONS" (`findmnt(8)`, which lists them as the default output).
const DEFAULT_COLUMNS: &[&str] = &["TARGET", "SOURCE", "FSTYPE", "OPTIONS"];

/// The column that names the mount POINT.
const TARGET_COLUMN: &str = "TARGET";

/// The column that names WHAT is mounted there.
const SOURCE_COLUMN: &str = "SOURCE";

/// The column that names the mount's filesystem TYPE.
const FSTYPE_COLUMN: &str = "FSTYPE";

/// The column that names the options it was mounted with.
const OPTIONS_COLUMN: &str = "OPTIONS";

/// The specifier `stat -f` writes the filesystem's type NAME for: "%T  type in human readable
/// form" (`stat(1)`).
const TYPE_NAME_SPECIFIER: &str = "%T";

/// The specifier either `stat` form writes the NAME it was asked about for: "%n  file name".
const FILE_NAME_SPECIFIER: &str = "%n";

/// The programs a guest asks about its own mounts with. They need no fact of the machine: what
/// is mounted into it is held by the session's own tree, which every program is handed.
#[must_use]
pub fn mount_programs() -> Box<dyn EnvironmentPrograms> {
    Box::new(MountPrograms)
}

/// See [`mount_programs`].
#[derive(Debug)]
struct MountPrograms;

impl EnvironmentPrograms for MountPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        // A program named by its absolute path is the same program, as a shell resolving a
        // name on `PATH` finds.
        let name = name.rsplit('/').next().unwrap_or(name);
        match name {
            FIND_MOUNT_PROGRAM => found_mount(arguments, &*files),
            STATUS_PROGRAM => file_system_status(arguments, &*files),
            _ => None,
        }
    }
}

/// Perform `findmnt`: write the columns asked for about the filesystem mounted at the path the
/// invocation names.
///
/// `None` for an invocation this program performs nothing of ([`FindMountInvocation`] states
/// which those are). Otherwise the status `findmnt` exits with: success for a path something is
/// mounted at, and [`NOTHING_MOUNTED`] for one nothing is — which is what makes the command the
/// way a script ASKS whether its mount is there.
fn found_mount(arguments: &[String], files: &dyn ProgramFiles) -> Option<ProgramOutcome> {
    let invocation = find_mount_invocation(arguments)?;
    let facts = match invocation.on_the_filesystem_of {
        // `-T` asks about the filesystem the path LIES ON, which is the mount holding it:
        // "if the path is not a mountpoint file or directory, findmnt checks path elements in
        // reverse order".
        true => files.file_system_at(&invocation.target),
        // The operand form matches a mount point itself, so a path that is not one is a path
        // nothing was found for.
        false => files.mounted_at(&invocation.target),
    };
    let Some(facts) = facts else {
        return Some(ProgramOutcome {
            status: NOTHING_MOUNTED,
            output: Vec::new(),
        });
    };
    let mut values: Vec<String> = Vec::new();
    for column in &invocation.columns {
        values.push(match column.as_str() {
            TARGET_COLUMN => invocation.target.clone(),
            SOURCE_COLUMN => facts.source.clone(),
            FSTYPE_COLUMN => facts.file_system_type.clone(),
            OPTIONS_COLUMN => facts.options.clone(),
            // A column this emulator derives nothing for is not answered with a blank: the
            // whole invocation is one this program performs nothing of.
            _ => return None,
        });
    }
    let mut output: Vec<String> = Vec::new();
    if invocation.headings {
        output.push(invocation.columns.join(" "));
    }
    output.push(values.join(" "));
    Some(ProgramOutcome {
        status: SUCCESS,
        output,
    })
}

/// Perform `stat -f`: write the format asked for about the FILESYSTEM the path lies on.
///
/// `None` for every other invocation of `stat` — the file-status form reports a file's own
/// mode, size and times, which this emulator holds none of — and for a format naming a
/// specifier it derives nothing for.
fn file_system_status(arguments: &[String], files: &dyn ProgramFiles) -> Option<ProgramOutcome> {
    let invocation = status_invocation(arguments)?;
    let Some(facts) = files.file_system_at(&invocation.target) else {
        // The machine's own storage: this emulator holds no filesystem facts for it, so
        // there is nothing to report rather than a type nothing stands behind.
        return Some(ProgramOutcome {
            status: CANNOT_STATUS,
            output: Vec::new(),
        });
    };
    let written = formatted_status(&invocation.format, &invocation.target, &facts)?;
    Some(ProgramOutcome {
        status: SUCCESS,
        output: vec![written],
    })
}

/// `format` with the specifiers this program derives substituted, or `None` when it names one
/// this emulator derives nothing for — which makes the whole invocation one this program
/// performs nothing of.
fn formatted_status(format: &str, target: &str, facts: &MountFacts) -> Option<String> {
    let mut written = String::new();
    let mut rest = format;
    while let Some(at) = rest.find('%') {
        written.push_str(&rest[..at]);
        let specifier = rest.get(at..at + 2)?;
        match specifier {
            TYPE_NAME_SPECIFIER => written.push_str(&facts.statfs_type),
            FILE_NAME_SPECIFIER => written.push_str(target),
            // `%%` is one literal per cent, the one specifier that names no value.
            "%%" => written.push('%'),
            _ => return None,
        }
        rest = &rest[at + 2..];
    }
    written.push_str(rest);
    Some(written)
}

/// One invocation of `findmnt` this program performs.
#[derive(Debug)]
struct FindMountInvocation {
    /// The path asked about.
    target: String,
    /// The columns to write, in the order the invocation named them.
    columns: Vec<String>,
    /// Whether a heading line is written, which `-n` is what leaves out.
    headings: bool,
    /// Whether the path was given as `-T`/`--target`, which asks about the filesystem the path
    /// LIES ON rather than about a mount point.
    on_the_filesystem_of: bool,
}

/// The `findmnt` invocation `arguments` describes, or `None` for one this program performs
/// nothing of: an option it does not read, and anything but the single-path form (the
/// invocation with no path at all lists every mount of the machine, which is a listing this
/// emulator does not write).
fn find_mount_invocation(arguments: &[String]) -> Option<FindMountInvocation> {
    let mut columns: Option<Vec<String>> = None;
    let mut headings = true;
    let mut on_the_filesystem_of = false;
    let mut operands: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            operands.extend(words.map(String::clone));
            break;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-o", "--output"]) {
            columns = Some(column_names(&named?));
            continue;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-T", "--target"]) {
            on_the_filesystem_of = true;
            operands.push(named?);
            continue;
        }
        if let Some(letters) = word.strip_prefix('-').filter(|word| !word.starts_with('-')) {
            // The letters of a grouped short option, which is how `-no FSTYPE` is written:
            // the group's LAST letter is the one that may take the value.
            let mut remaining = letters.chars().peekable();
            while let Some(letter) = remaining.next() {
                match letter {
                    'n' => headings = false,
                    'o' if remaining.peek().is_none() => {
                        columns = Some(column_names(&words.next()?.clone()));
                    }
                    'T' if remaining.peek().is_none() => {
                        on_the_filesystem_of = true;
                        operands.push(words.next()?.clone());
                    }
                    // Every other letter is one this program does not read.
                    _ => return None,
                }
            }
            continue;
        }
        if word == "--noheadings" {
            headings = false;
            continue;
        }
        if word.starts_with('-') {
            return None;
        }
        operands.push(word.clone());
    }
    let [target] = operands.as_slice() else {
        return None;
    };
    Some(FindMountInvocation {
        target: target.clone(),
        columns: columns.unwrap_or_else(|| {
            DEFAULT_COLUMNS
                .iter()
                .map(|column| (*column).to_string())
                .collect()
        }),
        headings,
        on_the_filesystem_of,
    })
}

/// The column names one `-o` value lists: comma-separated, and upper case whichever case the
/// invocation wrote them in, which is how `findmnt` reads them.
fn column_names(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_uppercase)
        .collect()
}

/// One invocation of `stat -f` this program performs.
#[derive(Debug)]
struct StatusInvocation {
    /// The path asked about.
    target: String,
    /// The format to write, as `-c` gave it.
    format: String,
}

/// The `stat -f` invocation `arguments` describes, or `None` for one this program performs
/// nothing of: the file-status form (without `-f`), an invocation with no format at all (whose
/// whole report this emulator derives nothing of), an option it does not read, and anything but
/// the single-path form.
fn status_invocation(arguments: &[String]) -> Option<StatusInvocation> {
    let mut file_system = false;
    let mut format: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            operands.extend(words.map(String::clone));
            break;
        }
        if let Some(named) =
            program_option_value(word, &mut words, &["-c", "--format", "--printf"])
        {
            format = Some(named?);
            continue;
        }
        if word == "-f" || word == "--file-system" {
            file_system = true;
            continue;
        }
        if let Some(letters) = word.strip_prefix('-').filter(|word| !word.starts_with('-')) {
            let mut remaining = letters.chars().peekable();
            while let Some(letter) = remaining.next() {
                match letter {
                    'f' => file_system = true,
                    // The last letter of the group is the one that may take the value.
                    'c' if remaining.peek().is_none() => {
                        format = Some(words.next()?.clone());
                    }
                    _ => return None,
                }
            }
            continue;
        }
        if word.starts_with('-') {
            return None;
        }
        operands.push(word.clone());
    }
    if !file_system {
        return None;
    }
    let [target] = operands.as_slice() else {
        return None;
    };
    Some(StatusInvocation {
        target: target.clone(),
        format: format?,
    })
}

#[cfg(test)]
mod tests {
    use ::service_common::posix_shell_volume::MountFacts;

    use super::{find_mount_invocation, formatted_status, status_invocation};

    /// One row of the `findmnt` table: the arguments written as a shell hands them over, and
    /// the invocation they describe — the path, the columns and whether headings are written
    /// — or `None` for one this program performs nothing of.
    type FindMountRow = (
        &'static str,
        &'static str,
        Option<(&'static str, Vec<&'static str>, bool)>,
    );

    /// One row of the `stat` table: the arguments, and the path and format they describe.
    type StatusRow = (&'static str, &'static str, Option<(&'static str, &'static str)>);
    /// The facts of a mounted network filesystem, as the program that mounted it states them.
    fn mounted() -> MountFacts {
        MountFacts {
            file_system_type: "nfs4".to_string(),
            statfs_type: "nfs".to_string(),
            source: "10.0.1.5:/".to_string(),
            options: "rw,nfsvers=4.1".to_string(),
        }
    }

    /// RULE: the `findmnt` invocations this program performs are the single-path ones whose
    /// columns it derives, in every spelling the options take.
    #[test]
    fn the_invocations_findmnt_performs_are_the_ones_it_derives() {
        // (case, the arguments, the path, the columns, whether headings are written)
        let rows: Vec<FindMountRow> = vec![
            (
                "the documented probe of a mount's type",
                "-no FSTYPE /mnt/efs",
                Some(("/mnt/efs", vec!["FSTYPE"], false)),
            ),
            (
                "the two options written apart",
                "-n -o FSTYPE /mnt/efs",
                Some(("/mnt/efs", vec!["FSTYPE"], false)),
            ),
            (
                "the long spelling of both",
                "--noheadings --output=fstype,source /mnt/efs",
                Some(("/mnt/efs", vec!["FSTYPE", "SOURCE"], false)),
            ),
            (
                "the headings kept",
                "-o TARGET,FSTYPE /mnt/efs",
                Some(("/mnt/efs", vec!["TARGET", "FSTYPE"], true)),
            ),
            (
                "no column named at all",
                "/mnt/efs",
                Some(("/mnt/efs", vec!["TARGET", "SOURCE", "FSTYPE", "OPTIONS"], true)),
            ),
            (
                "the path given as a target",
                "-n -o FSTYPE -T /mnt/efs/inside",
                Some(("/mnt/efs/inside", vec!["FSTYPE"], false)),
            ),
            ("no path at all", "-no FSTYPE", None),
            ("an option this program does not read", "-D /mnt/efs", None),
            ("a column value with no option", "-o", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let parsed = find_mount_invocation(&words);
            let reported = parsed.as_ref().map(|invocation| {
                (
                    invocation.target.as_str(),
                    invocation
                        .columns
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<&str>>(),
                    invocation.headings,
                )
            });
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: the `stat` invocations this program performs are the FILESYSTEM ones with a
    /// format, and the format is written with the specifiers it derives.
    #[test]
    fn the_invocations_stat_performs_are_the_filesystem_ones() {
        // (case, the arguments, the path and the format)
        let rows: Vec<StatusRow> = vec![
            (
                "the documented probe of a filesystem's type",
                "-f -c %T /mnt/efs",
                Some(("/mnt/efs", "%T")),
            ),
            (
                "the letters in one group",
                "-fc %T /mnt/efs",
                Some(("/mnt/efs", "%T")),
            ),
            (
                "the long spelling of both",
                "--file-system --format=%T /mnt/efs",
                Some(("/mnt/efs", "%T")),
            ),
            ("the file-status form", "-c %s /mnt/efs/file", None),
            ("no format at all", "-f /mnt/efs", None),
            ("no path at all", "-f -c %T", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let parsed = status_invocation(&words);
            let reported = parsed
                .as_ref()
                .map(|invocation| (invocation.target.as_str(), invocation.format.as_str()));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: a format writes the values this emulator derives and nothing else — a specifier it
    /// derives nothing for makes the whole invocation one the program performs nothing of.
    #[test]
    fn a_format_writes_only_the_values_this_emulator_derives() {
        // (case, the format, what it writes)
        let rows = [
            ("the type alone", "%T", Some("nfs")),
            ("the type with literal text around it", "type=%T.", Some("type=nfs.")),
            ("the name and the type", "%n is %T", Some("/mnt/efs is nfs")),
            ("a literal per cent", "100%%", Some("100%")),
            ("no specifier at all", "fixed", Some("fixed")),
            ("a specifier this emulator derives nothing for", "%b", None),
            ("a trailing per cent", "%", None),
        ];
        for (case, format, expected) in rows {
            let written = formatted_status(format, "/mnt/efs", &mounted());
            assert_eq!(
                written.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }
}
