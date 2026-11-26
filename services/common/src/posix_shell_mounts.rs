// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of a `mount(8)` / `umount(8)` command line, for every emulated service
//! whose file system a machine inside the emulator MOUNTS.
//!
//! # The rule this module implements
//!
//! A service whose data plane is a file system is reached by a program of a machine running
//! `mount`: the command names WHAT to mount, WHERE, and the options it is mounted with, and
//! the status it exits with says whether the mount happened ("32 mount failure", `mount(8)`'s
//! own EXIT STATUS). That grammar is the COMMAND's, not any one service's — two services whose
//! file systems a client mounts must not read `-t`, `-o`, the grouped flags or the
//! `<server>:<path>` source differently, and must not exit differently for the same invocation
//! — so it is read here once, beside the session that performs such a command
//! ([`crate::posix_shell_session`]) and the tree it mounts into
//! ([`crate::posix_shell_volume`]).
//!
//! What stays with each service is what only it knows: which filesystem TYPES its own data
//! plane serves (passed to each reader below), which address or resource a source names,
//! whether a source it cannot resolve is its own failure or a command another data plane
//! performs, and what the mount then REPORTS about itself
//! ([`crate::posix_shell_volume::MountFacts`]).
//!
//! Every reader is TOTAL over arbitrary words: the command line comes from a caller's own
//! script, so an invocation this module performs nothing of answers `None` — which the calling
//! program reports as a command that could not be run, rather than as a mount made under
//! options nothing applied. Nothing here opens a socket, starts a process or reaches a host
//! path.

use crate::posix_shell_session::program_option_value;

/// The program that mounts a filesystem, and the one that takes it away again.
pub const MOUNT_PROGRAM: &str = "mount";
pub const UNMOUNT_PROGRAM: &str = "umount";

/// The status a mount program exits with when it did what it was asked.
pub const SUCCESS: i32 = 0;

/// The status `mount` and `umount` exit with when the mount itself failed: "32 mount failure"
/// (`mount(8)` and `umount(8)`, EXIT STATUS).
pub const MOUNT_FAILURE: i32 = 32;

/// The options a mount reports when the command named none: a filesystem is mounted
/// read-write unless something says otherwise, which is the default `mount` applies.
pub const DEFAULT_MOUNT_OPTIONS: &str = "rw";

/// The export path a source names when it carries none at all, which is the file system's own
/// root — the path `<server>:` names.
pub const EXPORT_ROOT: &str = "/";

/// The separator between WHAT a mount source names and the path it exports, which is what
/// makes a source a network one: `<server>:/<path>`.
const SOURCE_SEPARATOR: char = ':';

/// The prefix of the MOUNT HELPER the kernel's `mount` calls for a filesystem type
/// (`/sbin/mount.<type>`), which a client may also invoke directly.
const MOUNT_HELPER_PREFIX: &str = "mount.";

/// The same for the unmount side (`/sbin/umount.<type>`).
const UNMOUNT_HELPER_PREFIX: &str = "umount.";

/// The option words `mount` and `umount` take whose meaning a program of this kind performs in
/// full, because they change nothing about WHICH filesystem is mounted where: how much the
/// command says about what it did, whether it records the mount in a table the machine does not
/// keep, and the read-write mode a mount has by default anyway.
const PERFORMED_FLAGS: &[&str] = &[
    "-v",
    "--verbose",
    "-n",
    "--no-mtab",
    "-w",
    "--rw",
    "--read-write",
    "-s",
    "--sloppy",
];

/// The option words `umount` takes beyond those: a detach this emulator performs at once
/// either way, and a forced one, which is the same detach when nothing is in flight.
const PERFORMED_UNMOUNT_FLAGS: &[&str] = &["-f", "--force", "-l", "--lazy", "-d", "--detach-loop"];

/// ONE invocation of `mount` such a program performs: what to mount, where, the options it was
/// asked for, and the filesystem type the command named, where it named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInvocation {
    /// The first operand: WHAT is mounted.
    pub source: String,
    /// The second operand: the directory it is mounted at.
    pub target: String,
    /// `-o`, as the command wrote it.
    pub options: Option<String>,
    /// `-t`, as the command wrote it — one of the types the caller said it serves, and `None`
    /// for a command that named none, whose source alone says what it mounts.
    pub file_system_type: Option<String>,
}

impl MountInvocation {
    /// The options the mount is made with: the ones the command named, or
    /// [`DEFAULT_MOUNT_OPTIONS`] for a command that named none.
    #[must_use]
    pub fn mount_options(&self) -> String {
        self.options
            .clone()
            .unwrap_or_else(|| DEFAULT_MOUNT_OPTIONS.to_string())
    }
}

/// ONE invocation of `umount` such a program performs: the mount point, and the filesystem
/// type the command named, where it named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmountInvocation {
    /// The one operand: the directory the filesystem is mounted at.
    pub target: String,
    /// `-t`, as the command wrote it.
    pub file_system_type: Option<String>,
}

/// Whether `name` invokes the program that MOUNTS one of the filesystem types `served`:
/// `mount` itself, or the helper the kernel calls for one of those types
/// (`/sbin/mount.<type>`), which a client may also invoke directly.
///
/// A program named by its absolute path is the same program, as a shell resolving a name on
/// `PATH` finds: `/usr/bin/mount` and `mount` are one.
#[must_use]
pub fn is_mount_program(name: &str, served: &[&str]) -> bool {
    let name = program_name(name);
    name == MOUNT_PROGRAM || helper_type(name, MOUNT_HELPER_PREFIX).is_some_and(|named| served.contains(&named))
}

/// [`is_mount_program`] for the program that takes a filesystem away again.
#[must_use]
pub fn is_unmount_program(name: &str, served: &[&str]) -> bool {
    let name = program_name(name);
    name == UNMOUNT_PROGRAM
        || helper_type(name, UNMOUNT_HELPER_PREFIX).is_some_and(|named| served.contains(&named))
}

/// The PROGRAM a command's first word names, with the directories of a path taken off it.
fn program_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// The filesystem type a helper's name carries, for a name that is one of those helpers.
fn helper_type<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix).filter(|named| !named.is_empty())
}

/// The `mount` invocation `arguments` describes, or `None` for one such a program performs
/// nothing of:
///
/// * a `-t` naming a filesystem type outside `served` — another filesystem's mount is not this
///   program's to make;
/// * an option it does not read, which is answered as a command that could not be run rather
///   than as a mount made under options nothing applied;
/// * anything but the two-operand form, since the forms with fewer operands mount what a table
///   of the machine says and the machines of this emulator keep none.
#[must_use]
pub fn mount_invocation(arguments: &[String], served: &[&str]) -> Option<MountInvocation> {
    let mut file_system_type: Option<String> = None;
    let mut options: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            operands.extend(words.map(String::clone));
            break;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-t", "--types"]) {
            file_system_type = Some(named?);
            continue;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-o", "--options"]) {
            options = Some(named?);
            continue;
        }
        if word.starts_with('-') {
            if !PERFORMED_FLAGS.contains(&word.as_str()) {
                return None;
            }
            continue;
        }
        operands.push(word.clone());
    }
    if file_system_type
        .as_deref()
        .is_some_and(|named| !served.contains(&named))
    {
        return None;
    }
    let [source, target] = operands.as_slice() else {
        return None;
    };
    Some(MountInvocation {
        source: source.clone(),
        target: target.clone(),
        options,
        file_system_type,
    })
}

/// The `umount` invocation `arguments` describes, or `None` for one such a program performs
/// nothing of — an option it does not read, a `-t` naming a filesystem type outside `served`,
/// or anything but the single-operand form.
#[must_use]
pub fn unmount_invocation(arguments: &[String], served: &[&str]) -> Option<UnmountInvocation> {
    let mut file_system_type: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            operands.extend(words.map(String::clone));
            break;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-t", "--types"]) {
            let named = named?;
            if !served.contains(&named.as_str()) {
                return None;
            }
            file_system_type = Some(named);
            continue;
        }
        if word.starts_with('-') {
            if !PERFORMED_FLAGS.contains(&word.as_str())
                && !PERFORMED_UNMOUNT_FLAGS.contains(&word.as_str())
            {
                return None;
            }
            continue;
        }
        operands.push(word.clone());
    }
    let [target] = operands.as_slice() else {
        return None;
    };
    Some(UnmountInvocation {
        target: target.clone(),
        file_system_type,
    })
}

/// WHAT a mount source names and the exported PATH it carries, or `None` for a source that
/// names no server at all — one carrying no `<server>:<path>` separator, which is a local
/// device rather than a filesystem of another component.
///
/// An address written in brackets is an IPv6 one, as a source carrying one spells it; a source
/// naming no path at all exports the filesystem's root, which is what `<server>:` means.
#[must_use]
pub fn mount_source(source: &str) -> Option<(String, String)> {
    let (named, path) = source.rsplit_once(SOURCE_SEPARATOR)?;
    let named = named
        .strip_prefix('[')
        .and_then(|inside| inside.strip_suffix(']'))
        .unwrap_or(named);
    if named.is_empty() {
        return None;
    }
    let path = if path.is_empty() { EXPORT_ROOT } else { path };
    Some((named.to_string(), path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The types ONE data plane of this workspace serves: its own, and the two spellings the
    /// kernel's client also takes for the same filesystem.
    const SERVED: &[&str] = &["s3files", "nfs4", "nfs"];

    /// One row of the mount table: the arguments written as a shell hands them over, and the
    /// invocation they describe — the source, the target, the options and the type — or `None`
    /// for one a program performs nothing of.
    type MountRow = (
        &'static str,
        &'static str,
        Option<(
            &'static str,
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
        )>,
    );

    /// RULE: the invocations a mount program performs are the two-operand mounts of the types
    /// it serves, in every spelling the options take; every other invocation is one it
    /// performs nothing of.
    #[test]
    fn only_a_two_operand_mount_of_a_served_type_is_performed() {
        // (case, the arguments, the source, the target, the options, the type)
        let rows: Vec<MountRow> = vec![
            (
                "the documented mount of a file system by its own id",
                "-t s3files fs-0123456789abcdef0:/ /mnt/data",
                Some((
                    "fs-0123456789abcdef0:/",
                    "/mnt/data",
                    None,
                    Some("s3files"),
                )),
            ),
            (
                "the documented mount of a network filesystem by address",
                "-t nfs4 -o nfsvers=4.1,rsize=1048576,hard 10.0.1.5:/ /mnt/efs",
                Some((
                    "10.0.1.5:/",
                    "/mnt/efs",
                    Some("nfsvers=4.1,rsize=1048576,hard"),
                    Some("nfs4"),
                )),
            ),
            (
                "the type and the options joined to their letters",
                "-ts3files -otls,iam fs-0123456789abcdef0:/ /mnt/data",
                Some((
                    "fs-0123456789abcdef0:/",
                    "/mnt/data",
                    Some("tls,iam"),
                    Some("s3files"),
                )),
            ),
            (
                "the long spelling of both options",
                "--types=nfs --options=rw 10.0.1.5:/sub /mnt/efs",
                Some(("10.0.1.5:/sub", "/mnt/efs", Some("rw"), Some("nfs"))),
            ),
            (
                "no type at all, which the source itself names",
                "10.0.1.5:/ /mnt/efs",
                Some(("10.0.1.5:/", "/mnt/efs", None, None)),
            ),
            (
                "a flag this program performs in full",
                "-v -t nfs4 10.0.1.5:/ /mnt/efs",
                Some(("10.0.1.5:/", "/mnt/efs", None, Some("nfs4"))),
            ),
            (
                "the operands written after the end of the options",
                "-t nfs4 -- 10.0.1.5:/ /mnt/efs",
                Some(("10.0.1.5:/", "/mnt/efs", None, Some("nfs4"))),
            ),
            (
                "a type this data plane does not serve",
                "-t ext4 /dev/nvme1n1 /mnt/disk",
                None,
            ),
            (
                "an option this program does not read",
                "-r 10.0.1.5:/ /mnt/efs",
                None,
            ),
            ("one operand only", "-t nfs4 10.0.1.5:/", None),
            ("three operands", "-t nfs4 a:/ /mnt/efs /mnt/other", None),
            ("no operand at all", "-t nfs4", None),
            ("a type option with no value", "-t", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let parsed = mount_invocation(&words, SERVED);
            let reported = parsed.as_ref().map(|invocation| {
                (
                    invocation.source.as_str(),
                    invocation.target.as_str(),
                    invocation.options.as_deref(),
                    invocation.file_system_type.as_deref(),
                )
            });
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: the options a mount is made with are the ones the command named, and a command
    /// that named none mounts read-write, which is `mount`'s own default.
    #[test]
    fn a_mount_that_named_no_options_is_made_read_write() {
        // (case, the arguments, the options the mount reports)
        let rows = [
            ("options the command named", "-o tls 10.0.1.5:/ /mnt", "tls"),
            (
                "no options at all",
                "10.0.1.5:/ /mnt",
                DEFAULT_MOUNT_OPTIONS,
            ),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let reported = mount_invocation(&words, SERVED)
                .map(|invocation| invocation.mount_options())
                .unwrap_or_default();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: `umount` takes ONE operand — the mount point — beside the flags whose effect such
    /// a program performs, and nothing else is an invocation it performs.
    #[test]
    fn umount_takes_the_one_mount_point() {
        // (case, the arguments, the mount point and the type)
        let rows = [
            ("the mount point alone", "/mnt/data", Some(("/mnt/data", None))),
            ("a flag beside it", "-l /mnt/data", Some(("/mnt/data", None))),
            (
                "the type narrowed to a served one",
                "-t s3files /mnt/data",
                Some(("/mnt/data", Some("s3files"))),
            ),
            ("the type narrowed to another", "-t ext4 /mnt/disk", None),
            (
                "an option this program does not read",
                "-O remount /mnt/data",
                None,
            ),
            ("every mount point at once", "-a", None),
            ("two mount points", "/mnt/data /mnt/other", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let parsed = unmount_invocation(&words, SERVED);
            let reported = parsed.as_ref().map(|invocation| {
                (
                    invocation.target.as_str(),
                    invocation.file_system_type.as_deref(),
                )
            });
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: a mount source names WHAT is mounted and the path that server exports, and a
    /// source naming no path exports the filesystem's root.
    #[test]
    fn a_source_names_the_server_and_the_path_it_exports() {
        // (case, the source, what it names and the exported path)
        let rows = [
            ("the root of the filesystem", "10.0.1.5:/", Some(("10.0.1.5", "/"))),
            (
                "a directory of it",
                "10.0.1.5:/team/data",
                Some(("10.0.1.5", "/team/data")),
            ),
            ("no path at all", "10.0.1.5:", Some(("10.0.1.5", "/"))),
            ("an IPv6 address", "[fd12:3456::1]:/", Some(("fd12:3456::1", "/"))),
            (
                "a file system named by its own id",
                "fs-0123456789abcdef0:/",
                Some(("fs-0123456789abcdef0", "/")),
            ),
            (
                "a host name",
                "fs-0.efs.us-east-1.amazonaws.com:/",
                Some(("fs-0.efs.us-east-1.amazonaws.com", "/")),
            ),
            ("a local device", "/dev/nvme1n1", None),
            ("a source naming nothing", ":/", None),
        ];
        for (case, source, expected) in rows {
            let parsed = mount_source(source);
            let reported = parsed
                .as_ref()
                .map(|(named, path)| (named.as_str(), path.as_str()));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: the programs that mount and unmount are `mount` and `umount` themselves and the
    /// helpers the kernel calls for the types a data plane serves, by name or by path — and no
    /// other program at all.
    #[test]
    fn the_mount_programs_are_the_command_and_the_helpers_of_the_served_types() {
        // (case, the name as the command wrote it, mounts, unmounts)
        let rows = [
            ("the command itself", "mount", true, false),
            ("the command by path", "/usr/bin/mount", true, false),
            ("the unmount command", "umount", false, true),
            ("the helper of a served type", "mount.s3files", true, false),
            ("the helper by path", "/sbin/mount.nfs4", true, false),
            ("the unmount helper", "umount.nfs", false, true),
            ("the helper of a type nothing serves", "mount.ext4", false, false),
            ("another program entirely", "findmnt", false, false),
        ];
        for (case, name, mounts, unmounts) in rows {
            assert_eq!(
                (is_mount_program(name, SERVED), is_unmount_program(name, SERVED)),
                (mounts, unmounts),
                "case {case:?}: expected {name:?} to mount = {mounts} and unmount = {unmounts}"
            );
        }
    }
}
