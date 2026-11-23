// SPDX-License-Identifier: CC0-1.0

//! The NATIVE data plane of this service: the NFS server a mount target answers on, and the
//! client programs a machine inside the VPC mounts it with.
//!
//! # The rule this module implements
//!
//! A mount target is the ADDRESS a file system is reached at: "after creating the mount
//! target, Amazon EFS returns a response that includes a MountTargetId and an IpAddress. You
//! use this IP address when mounting the file system in an EC2 instance"
//! (`CreateMountTarget`), and the lifecycle state the control plane reports for it is what
//! says the address is serving — a mount target is `available` once "the mount target is
//! ready for use" (`MountTargetDescription$LifeCycleState`). So a mount target reported
//! `available` answers a client mounting it at the address it reports, and one still
//! `creating` answers nothing: that is the whole promise `available` carries, and a control
//! plane that reports it while nothing answers has handed the client a file system it cannot
//! use.
//!
//! What is reached THROUGH that address is the file system's own tree, held beside the record
//! that names it ([`super::file_system_data`]) — never a copy — so what one client writes
//! through its own mount, every other client that mounted the same file system reads through
//! theirs. That is what makes the file system "a file system for use with Amazon Web Services
//! cloud services" rather than per-client scratch space, and it is why the mount hands the
//! session the one shared tree ([`MountedFileSystem::mounted_tree`]) instead of seeding it
//! with a copy.
//!
//! An address is reachable only from INSIDE the VPC the mount target was placed in: the mount
//! target holds a network interface in a subnet of that VPC, and its address is a private
//! address of it. So the exports a client is offered here are the mount targets of the VPC
//! that client's own machine is in, and a client this emulator cannot place in a VPC at all
//! is offered none — a mount of an address nothing answers from where the client stands fails
//! the way `mount` fails, rather than succeeding against a file system the client has no
//! network path to.
//!
//! # Who runs these programs, and where
//!
//! Mounting happens INSIDE a machine, and the machine belongs to the service that owns
//! instances: the bounded, in-process shell session a command delivered to that machine is
//! performed in is that service's ([`::service_common::posix_shell_session`]). The component
//! that reaches into the machine SUPPLIES the programs only it can answer for, so the
//! programs below are handed to that session by the caller that delivers the script — the
//! file system, the mount target and the address it serves at are facts of THIS service, and
//! the service that owns the machine holds none of them.
//!
//! Nothing here opens a socket, starts a process or reads a host path: an export is a record
//! this store holds, a mount is an entry in the session's own mount table
//! ([`::service_common::posix_shell_volume::ShellVolume::mount`]), and the files behind it are
//! the ones this process holds for the file system. The programs answer only the invocations
//! they really perform; one that names an option or a filesystem type nothing here implements
//! answers as a command that could not be run, rather than as a mount nothing made.

use std::time::SystemTime;

use ::service_common::posix_shell_session::{
    program_option_value, EnvironmentPrograms, ProgramFiles, ProgramOutcome,
};
use ::service_common::posix_shell_volume::MountFacts;

use super::{service_state, MountedFileSystem, LIFE_CYCLE_AVAILABLE};

/// The port an NFS server answers on, which is the port a mount target's security groups have
/// to admit for a client to reach it: "you need to allow inbound traffic on TCP port 2049"
/// (Amazon EFS User Guide, "Creating security groups").
pub const NFS_PORT: u16 = 2049;

/// The filesystem TYPE an Amazon EFS mount is made as — "NFSv4.1 … `mount -t nfs4`" — which
/// is what a listing of the machine's mounts reports for it.
pub const NFS4_FILE_SYSTEM_TYPE: &str = "nfs4";

/// The other spelling of the type a `-t` option may name for the same filesystem, which the
/// kernel's NFS client also serves.
const NFS_FILE_SYSTEM_TYPE: &str = "nfs";

/// The type a statfs of a path on such a mount reports — the name `stat -f`'s `%T` writes for
/// the NFS magic number (`coreutils`' own table: `NFS_SUPER_MAGIC` is `nfs`), which is not the
/// same string as the mount type above.
pub const NFS_STATFS_TYPE: &str = "nfs";

/// The program that mounts a filesystem, and the one that takes it away again.
const MOUNT_PROGRAM: &str = "mount";
const UNMOUNT_PROGRAM: &str = "umount";

/// The mount HELPERS the kernel's mount calls for these filesystem types
/// (`/sbin/mount.<type>`), which a client may also invoke directly.
const MOUNT_HELPERS: &[&str] = &["mount.nfs", "mount.nfs4"];

/// See [`MOUNT_HELPERS`].
const UNMOUNT_HELPERS: &[&str] = &["umount.nfs", "umount.nfs4"];

/// The status a program exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status `mount` and `umount` exit with when the mount itself failed: "32 mount failure"
/// (`mount(8)` and `umount(8)`, EXIT STATUS).
const MOUNT_FAILURE: i32 = 32;

/// The options a mount reports when the command named none: a filesystem is mounted
/// read-write unless something says otherwise, which is the default `mount` applies.
const DEFAULT_MOUNT_OPTIONS: &str = "rw";

/// The export path a source names when it carries none at all, which is the file system's own
/// root — the path `<address>:/` names.
const EXPORT_ROOT: &str = "/";

/// The separator between the ADDRESS of an NFS server and the path it exports, which is what
/// makes a mount source an NFS one: `<address>:/<path>`.
const SOURCE_SEPARATOR: char = ':';

/// The option words `mount` and `umount` take whose meaning this program performs in full,
/// because they change nothing about WHICH filesystem is mounted where: how much the command
/// says about what it did, whether it records the mount in a table this machine does not keep,
/// and the read-write mode a mount has by default anyway.
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

/// ONE export this emulator's NFS server offers: the address it answers at, the file system
/// behind it and the files that file system holds.
#[derive(Debug, Clone)]
struct NfsExport {
    /// The address a client mounts it by — `MountTargetDescription$IpAddress` or its
    /// `$Ipv6Address`, which is the address the control plane told the client to use.
    address: String,
    /// The mount target serving it, which is the record whose lifecycle state said the
    /// address is serving.
    mount_target_id: String,
    /// The file system reached through it.
    file_system_id: String,
    /// The files that file system holds, shared with every other client that mounted it.
    files: MountedFileSystem,
}

/// The NFS client of ONE machine: the exports reachable from where that machine stands, and
/// the programs it mounts them with.
#[derive(Debug, Clone)]
pub struct NfsClient {
    /// The exports this client can reach — the mount targets of its own VPC.
    exports: Vec<NfsExport>,
}

/// The NFS client programs for a machine in `client_vpc_id`, as that machine's session
/// performs them — and `None` when this engine holds no store for this service, which is an
/// emulator with no Amazon EFS beside the machine at all.
///
/// The exports are read at the instant the run starts: the transitions the clock has already
/// made due are materialised first, because a mount target whose `creating` span has elapsed
/// is `available` whether or not anybody called, and only the `available` ones answer. A
/// client this caller could place in no VPC reaches none of them.
///
/// LOCK ORDER: this takes THIS store's lock, holds no other while it does, and hands back
/// owned handles rather than a guard, so a caller may call it with no guard of its own store
/// held.
#[must_use]
pub fn nfs_client_programs<Eng: Send + Sync + 'static>(
    engine: &Eng,
    now: SystemTime,
    client_vpc_id: Option<&str>,
) -> Option<Box<dyn EnvironmentPrograms>> {
    let state = service_state(engine)?;
    let mut store = state.lock();
    // The move to `available` every mount target was already owed: owed at its own instant
    // whether or not anybody called, and it is what decides whether the address serves.
    store.materialize_due_transitions(now);
    let serving: Vec<(String, String, String)> = store
        .mount_targets
        .values()
        .filter(|mount_target| mount_target.life_cycle_state == LIFE_CYCLE_AVAILABLE)
        .filter(|mount_target| client_vpc_id == Some(mount_target.vpc_id.as_str()))
        .flat_map(|mount_target| {
            mount_target
                .ip_address
                .iter()
                .chain(mount_target.ipv6_address.iter())
                .map(|address| {
                    (
                        address.clone(),
                        mount_target.mount_target_id.clone(),
                        mount_target.file_system_id.clone(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let mut exports: Vec<NfsExport> = Vec::new();
    for (address, mount_target_id, file_system_id) in serving {
        // The files are the FILE SYSTEM's own, handed out as the shared handle every client
        // of it is given; a file system this store no longer holds offers nothing.
        if let Some(files) = store.mounted_file_system(&file_system_id) {
            exports.push(NfsExport {
                address,
                mount_target_id,
                file_system_id,
                files,
            });
        }
    }
    Some(Box::new(NfsClient { exports }))
}

impl EnvironmentPrograms for NfsClient {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        // A program named by its absolute path is the same program, as a shell resolving a
        // name on `PATH` finds: `/usr/bin/mount` and `mount` are one.
        let name = name.rsplit('/').next().unwrap_or(name);
        if name == MOUNT_PROGRAM || MOUNT_HELPERS.contains(&name) {
            return self.mounted(arguments, files);
        }
        if name == UNMOUNT_PROGRAM || UNMOUNT_HELPERS.contains(&name) {
            return unmounted(arguments, files);
        }
        None
    }
}

impl NfsClient {
    /// Perform `mount`: attach the file system the source names at the directory the target
    /// names, so every path under it is a path of that file system from then on.
    ///
    /// `None` for an invocation this program performs nothing of ([`mount_invocation`] states
    /// which those are). Otherwise the status `mount` exits with: success for a mount that was
    /// made, and [`MOUNT_FAILURE`] for one that was not — nothing answers NFS at that address
    /// from where this machine stands, the file system exports no such path, or the machine
    /// has no directory to mount it over.
    fn mounted(
        &self,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let invocation = mount_invocation(arguments)?;
        let (address, export_path) = nfs_source(&invocation.source)?;
        let failed = Some(ProgramOutcome {
            status: MOUNT_FAILURE,
            output: Vec::new(),
        });
        // The address has to be one an export of this client's own VPC answers at: that is
        // what "the NFS server is reachable at the mount target's address from inside the
        // VPC" means, and no other address answers.
        let Some(export) = self
            .exports
            .iter()
            .find(|export| export.address == address)
        else {
            return failed;
        };
        let facts = MountFacts {
            file_system_type: NFS4_FILE_SYSTEM_TYPE.to_string(),
            statfs_type: NFS_STATFS_TYPE.to_string(),
            source: invocation.source.clone(),
            options: invocation
                .options
                .clone()
                .unwrap_or_else(|| DEFAULT_MOUNT_OPTIONS.to_string()),
        };
        let Some(tree) = export.files.mounted_tree(&export_path, facts) else {
            return failed;
        };
        if !files.mount(&invocation.target, tree) {
            return failed;
        }
        Some(ProgramOutcome {
            status: SUCCESS,
            output: Vec::new(),
        })
    }

    /// Which file system is reached through the export at `address`, and the mount target
    /// serving it — what a caller that has to report the mount it made reads.
    #[must_use]
    pub fn export_at(&self, address: &str) -> Option<(&str, &str)> {
        self.exports
            .iter()
            .find(|export| export.address == address)
            .map(|export| {
                (
                    export.file_system_id.as_str(),
                    export.mount_target_id.as_str(),
                )
            })
    }
}

/// Perform `umount`: take the filesystem mounted at the target away again, leaving the
/// directory it was mounted over as it was.
///
/// `None` for an invocation this program performs nothing of; otherwise success for a
/// filesystem that was mounted there and [`MOUNT_FAILURE`] for a target nothing was — which is
/// what `umount` answers for a path that is not a mount point.
fn unmounted(arguments: &[String], files: &mut dyn ProgramFiles) -> Option<ProgramOutcome> {
    let target = unmount_target(arguments)?;
    let status = if files.unmount(&target) {
        SUCCESS
    } else {
        MOUNT_FAILURE
    };
    Some(ProgramOutcome {
        status,
        output: Vec::new(),
    })
}

/// One invocation of `mount` this program performs: what to mount, where, and the options it
/// was asked for.
#[derive(Debug)]
struct MountInvocation {
    /// The first operand: WHAT is mounted.
    source: String,
    /// The second operand: the directory it is mounted at.
    target: String,
    /// `-o`, as the command wrote it.
    options: Option<String>,
}

/// The invocation `arguments` describes, or `None` for one this program performs nothing of:
///
/// * a `-t` naming a filesystem type other than this service's own — another filesystem's
///   mount is not this program's to make;
/// * an option this program does not read, which is answered as a command that could not be
///   run rather than as a mount made under options nothing applied;
/// * anything but the two-operand form, since the forms with fewer operands mount what a
///   table of this machine says and this machine keeps none.
fn mount_invocation(arguments: &[String]) -> Option<MountInvocation> {
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
    if let Some(named) = file_system_type.as_deref() {
        if named != NFS4_FILE_SYSTEM_TYPE && named != NFS_FILE_SYSTEM_TYPE {
            return None;
        }
    }
    let [source, target] = operands.as_slice() else {
        return None;
    };
    Some(MountInvocation {
        source: source.clone(),
        target: target.clone(),
        options,
    })
}

/// The one operand `umount` was given, or `None` for an invocation this program performs
/// nothing of — an option it does not read, or anything but the single-operand form.
fn unmount_target(arguments: &[String]) -> Option<String> {
    let mut operands: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            operands.extend(words.map(String::clone));
            break;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-t", "--types"]) {
            let named = named?;
            if named != NFS4_FILE_SYSTEM_TYPE && named != NFS_FILE_SYSTEM_TYPE {
                return None;
            }
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
    Some(target.clone())
}

/// The ADDRESS and the exported PATH a mount source names, or `None` for a source that is not
/// an NFS one at all — one carrying no `<address>:<path>` separator, which is a local device
/// rather than a server.
///
/// An address written in brackets is an IPv6 one, as a source carrying one spells it; a source
/// naming no path at all exports the file system's root, which is what `<address>:` means.
fn nfs_source(source: &str) -> Option<(String, String)> {
    let (address, path) = source.rsplit_once(SOURCE_SEPARATOR)?;
    let address = address
        .strip_prefix('[')
        .and_then(|inside| inside.strip_suffix(']'))
        .unwrap_or(address);
    if address.is_empty() {
        return None;
    }
    let path = if path.is_empty() { EXPORT_ROOT } else { path };
    Some((address.to_string(), path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{mount_invocation, nfs_source, unmount_target};

    /// The invocation one argument list describes, for a table written in shell words.
    fn invocation(line: &str) -> Option<(String, String, Option<String>)> {
        let arguments: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        mount_invocation(&arguments)
            .map(|parsed| (parsed.source, parsed.target, parsed.options))
    }

    /// One row of the table below: the arguments written as a shell hands them over, and the
    /// invocation they describe — the source, the target and the options — or `None` for one
    /// this program performs nothing of.
    type InvocationRow = (&'static str, &'static str, Option<(&'static str, &'static str, Option<&'static str>)>);

    /// RULE: the invocations this program performs are the two-operand mounts of its own
    /// filesystem type, in every spelling the options take; every other invocation is one it
    /// performs nothing of.
    #[test]
    fn only_a_two_operand_mount_of_this_filesystem_is_performed() {
        // (case, the arguments, the source, the target, the options)
        let rows: Vec<InvocationRow> = vec![
            (
                "the documented Amazon EFS mount",
                "-t nfs4 -o nfsvers=4.1,rsize=1048576,hard 10.0.1.5:/ /mnt/efs",
                Some((
                    "10.0.1.5:/",
                    "/mnt/efs",
                    Some("nfsvers=4.1,rsize=1048576,hard"),
                )),
            ),
            (
                "the type and the options joined to their letters",
                "-tnfs4 -onfsvers=4.1 10.0.1.5:/ /mnt/efs",
                Some(("10.0.1.5:/", "/mnt/efs", Some("nfsvers=4.1"))),
            ),
            (
                "the long spelling of both options",
                "--types=nfs --options=rw 10.0.1.5:/sub /mnt/efs",
                Some(("10.0.1.5:/sub", "/mnt/efs", Some("rw"))),
            ),
            (
                "no type at all, which the source itself names",
                "10.0.1.5:/ /mnt/efs",
                Some(("10.0.1.5:/", "/mnt/efs", None)),
            ),
            (
                "a flag this program performs in full",
                "-v -t nfs4 10.0.1.5:/ /mnt/efs",
                Some(("10.0.1.5:/", "/mnt/efs", None)),
            ),
            (
                "another filesystem's type",
                "-t ext4 /dev/nvme1n1 /mnt/disk",
                None,
            ),
            ("an option this program does not read", "-r 10.0.1.5:/ /mnt/efs", None),
            ("one operand only", "-t nfs4 10.0.1.5:/", None),
            ("three operands", "-t nfs4 a:/ /mnt/efs /mnt/other", None),
            ("no operand at all", "-t nfs4", None),
            ("a type option with no value", "-t", None),
        ];
        for (case, arguments, expected) in rows {
            let parsed = invocation(arguments);
            let reported = parsed.as_ref().map(|(source, target, options)| {
                (source.as_str(), target.as_str(), options.as_deref())
            });
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: a mount source is an NFS one when it names an ADDRESS and the path that server
    /// exports, and a source naming no path exports the file system's root.
    #[test]
    fn a_source_names_the_server_and_the_path_it_exports() {
        // (case, the source, the address and the exported path)
        let rows = [
            ("the root of the file system", "10.0.1.5:/", Some(("10.0.1.5", "/"))),
            ("a directory of it", "10.0.1.5:/team/data", Some(("10.0.1.5", "/team/data"))),
            ("no path at all", "10.0.1.5:", Some(("10.0.1.5", "/"))),
            ("an IPv6 address", "[fd12:3456::1]:/", Some(("fd12:3456::1", "/"))),
            ("a host name", "fs-0.efs.us-east-1.amazonaws.com:/", Some(("fs-0.efs.us-east-1.amazonaws.com", "/"))),
            ("a local device", "/dev/nvme1n1", None),
            ("a source with no address", ":/", None),
        ];
        for (case, source, expected) in rows {
            let parsed = nfs_source(source);
            let reported = parsed
                .as_ref()
                .map(|(address, path)| (address.as_str(), path.as_str()));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: `umount` takes ONE operand — the mount point — beside the flags whose effect this
    /// program performs, and nothing else is an invocation it performs.
    #[test]
    fn umount_takes_the_one_mount_point() {
        // (case, the arguments, the target)
        let rows = [
            ("the mount point alone", "/mnt/efs", Some("/mnt/efs")),
            ("a flag beside it", "-l /mnt/efs", Some("/mnt/efs")),
            ("the type narrowed to this filesystem", "-t nfs4 /mnt/efs", Some("/mnt/efs")),
            ("the type narrowed to another", "-t ext4 /mnt/disk", None),
            ("an option this program does not read", "-O remount /mnt/efs", None),
            ("every mount point at once", "-a", None),
            ("two mount points", "/mnt/efs /mnt/other", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let target = unmount_target(&words);
            assert_eq!(
                target.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {target:?}"
            );
        }
    }
}
