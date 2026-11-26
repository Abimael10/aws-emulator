// SPDX-License-Identifier: CC0-1.0

//! The NATIVE data plane of this service: the file system a mount target serves, and the
//! client programs a machine inside the VPC mounts it with.
//!
//! # The rule this module implements
//!
//! A mount target is the endpoint a file system is reached at: `CreateMountTarget` "creates a
//! mount target resource as an endpoint for mounting the S3 File System from compute
//! resources" in one subnet, and the lifecycle state the control plane reports for it is what
//! says the endpoint is serving — `available` is the state a created resource of this service
//! carries "once it is usable". So a file system with a mount target reported `available` is
//! one a client in that mount target's VPC can MOUNT, and a file system whose mount target is
//! still `creating` serves nothing: that is the whole promise `available` carries, and a
//! control plane that reports it while nothing answers has handed the client a file system it
//! cannot use.
//!
//! What is reached THROUGH it is the file system's own tree, held beside the record that names
//! it ([`super::file_data`]) — never a copy — so what one client writes through its own mount,
//! every other client that mounted the same file system reads through theirs, and every file
//! of that tree is an object of the bucket the file system is a view of
//! ([`super::BucketWriteThrough`]).
//!
//! An endpoint is reachable only from INSIDE the VPC its mount target was placed in: the mount
//! target holds a network interface in a subnet of that VPC, and the address it reports is a
//! private address of it. So the file systems a client is offered here are the ones with a
//! mount target in the VPC that client's own machine is in, and a client this emulator cannot
//! place in a VPC at all is offered none — a mount of something nothing answers from where the
//! client stands fails the way `mount` fails, rather than succeeding against a file system the
//! client has no network path to.
//!
//! # How a client names what it mounts
//!
//! The documented mount of an S3 File System names the FILE SYSTEM — `mount -t s3files
//! <fileSystemId>:/ <dir>`, performed by the mount helper the file-system utilities package
//! ships — and the address the control plane reports for the mount target is the other way to
//! name the same endpoint, the way any NFS mount names a server. Both are accepted here
//! ([`S3FilesClient::export_named`]), together with a host name whose first label is the file
//! system's id, which is how a name that resolves to the mount target of the client's own zone
//! is written.
//!
//! The COMMAND LINE itself — which program was invoked, the `-t`, the `-o`, the flags and the
//! `<server>:<path>` source — is read by the one reading of a `mount(8)` invocation this
//! workspace has ([`::service_common::posix_shell_mounts`]), narrowed to the filesystem types
//! this data plane serves ([`SERVED_MOUNT_TYPES`]). What stays here is only what this service
//! knows: which file system a source names, and whether a source it cannot resolve is this
//! program's failure or a command another data plane performs.
//!
//! # Who runs these programs, and where
//!
//! Mounting happens INSIDE a machine, and the machine belongs to the service that owns
//! instances: the bounded, in-process shell session a command delivered to that machine is
//! performed in is that service's ([`::service_common::posix_shell_session`]). The component
//! that reaches into the machine SUPPLIES the programs only it can answer for, so the programs
//! below are handed to that session by the caller that delivers the script — the file system,
//! its mount targets and the files it holds are facts of THIS service, and the service that
//! owns the machine holds none of them.
//!
//! Nothing here opens a socket, starts a process or reads a host path: an export is a record
//! this store holds, a mount is an entry in the session's own mount table
//! ([`::service_common::posix_shell_volume::ShellVolume::mount`]), and the files behind it are
//! the ones this process holds for the file system. The programs answer only the invocations
//! they really perform; one that names an option this emulator does not read answers as a
//! command that could not be run, rather than as a mount nothing made.

use std::time::SystemTime;

use ::service_common::posix_shell_mounts::{
    is_mount_program, is_unmount_program, mount_invocation, mount_source, unmount_invocation,
    MOUNT_FAILURE, SUCCESS,
};
use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};
use ::service_common::posix_shell_volume::MountFacts;

use super::{service_state, BucketWriteThrough, MountedFileSystem, STATUS_AVAILABLE};

/// The filesystem TYPE the documented mount of an S3 File System names — `mount -t s3files`,
/// which the file-system utilities package's own mount helper performs.
pub const S3FILES_MOUNT_TYPE: &str = "s3files";

/// The TYPE the mount reports once it is made, which is the type the kernel holds it as: the
/// helper mounts the file system over NFSv4.1, so a listing of the machine's mounts reports
/// `nfs4` for it.
pub const MOUNTED_FILE_SYSTEM_TYPE: &str = "nfs4";

/// The type a statfs of a path on such a mount reports — the name `stat -f`'s `%T` writes for
/// the NFS magic number (`coreutils`' own table: `NFS_SUPER_MAGIC` is `nfs`), which is not the
/// same string as the mount type above.
pub const MOUNTED_STATFS_TYPE: &str = "nfs";

/// The filesystem types a `-t` may name for THIS data plane's mount: the service's own type,
/// and the two spellings the kernel's NFS client also takes for the same endpoint — a client
/// that mounts it as a plain NFS server writes one of those.
///
/// It is also what decides which mount HELPERS are this program's
/// ([`::service_common::posix_shell_mounts::is_mount_program`]).
const SERVED_MOUNT_TYPES: &[&str] = &[
    S3FILES_MOUNT_TYPE,
    MOUNTED_FILE_SYSTEM_TYPE,
    MOUNTED_STATFS_TYPE,
];

/// Whether a command that named `file_system_type` meant THIS service's own file system, which
/// is what decides whether a source this client cannot resolve is this program's failure or a
/// command it performs nothing of.
///
/// Only the service's own type says so: the NFS spellings name a filesystem another data plane
/// may serve just as well, and a command that named no type at all says nothing about which.
fn named_this_file_system(file_system_type: Option<&str>) -> bool {
    file_system_type == Some(S3FILES_MOUNT_TYPE)
}

/// ONE export this emulator's file-system endpoint offers: the file system behind it, the
/// addresses it answers at and the files it holds.
#[derive(Debug, Clone)]
struct Export {
    /// The file system reached through it.
    file_system_id: String,
    /// The addresses the mount target serving it reports —
    /// `CreateMountTargetResponse$ipv4Address` and its `$ipv6Address`, which is what the
    /// control plane told the client to use.
    addresses: Vec<String>,
    /// The files the file system holds, shared with every other client that mounted it.
    files: MountedFileSystem,
}

/// The file-system client of ONE machine: the exports reachable from where that machine
/// stands, and the programs it mounts them with.
#[derive(Debug, Clone)]
pub struct S3FilesClient {
    /// The exports this client can reach — the file systems with a mount target in its own
    /// VPC.
    exports: Vec<Export>,
}

/// The file-system client programs for a machine in `client_vpc_id`, as that machine's session
/// performs them — and `None` when this engine holds no store for this service, which is an
/// emulator with no S3 Files beside the machine at all.
///
/// The exports are read at the instant the run starts: the transitions the clock has already
/// made due are materialised first, because a mount target whose `creating` span has elapsed
/// is `available` whether or not anybody called, and only an `available` mount target of an
/// `available` file system serves. A client this caller could place in no VPC reaches none of
/// them.
///
/// The WRITE-THROUGH of each file system is resolved here too ([`BucketWriteThrough`]), on the
/// first mount of that file system: a write that lands in the tree is an object of the bucket
/// the file system is a view of, and the object store that holds it is reached on this same
/// engine.
///
/// LOCK ORDER: this takes THIS store's lock, holds no other while it does, and hands back
/// owned handles rather than a guard, so a caller may call it with no guard of its own store
/// held.
#[must_use]
pub fn mount_client_programs<Eng: Send + Sync + 'static>(
    engine: &Eng,
    now: SystemTime,
    client_vpc_id: Option<&str>,
) -> Option<Box<dyn EnvironmentPrograms>> {
    let state = service_state(engine)?;
    // What SERVES, read under the guard and handed out as owned records: the file system
    // behind each `available` mount target of the client's own VPC.
    let serving: Vec<(super::FileSystem, Vec<String>)> = {
        let mut store = state.lock();
        // The move to `available` every resource was already owed: owed at its own instant
        // whether or not anybody called, and it is what decides whether the endpoint serves.
        store.settle(now);
        store
            .mount_targets
            .values()
            .filter(|mount_target| mount_target.status == STATUS_AVAILABLE)
            .filter(|mount_target| client_vpc_id == Some(mount_target.vpc_id.as_str()))
            .filter_map(|mount_target| {
                let file_system = store.file_systems.get(&mount_target.file_system_id)?;
                if file_system.status != STATUS_AVAILABLE {
                    return None;
                }
                Some((
                    file_system.clone(),
                    mount_target
                        .ipv4_address
                        .iter()
                        .chain(mount_target.ipv6_address.iter())
                        .cloned()
                        .collect(),
                ))
            })
            .collect()
    };
    let mut exports: Vec<Export> = Vec::new();
    for (file_system, addresses) in serving {
        // The write-through is resolved with NO guard of this store held: it reaches the
        // object store on this same engine, which is another service's.
        let writes = BucketWriteThrough::of(engine, &file_system);
        // The files are the FILE SYSTEM's own, handed out as the shared handle every client
        // of it is given; a file system this store no longer holds offers nothing.
        let files = state
            .lock()
            .mounted_file_system(&file_system.file_system_id, writes);
        if let Some(files) = files {
            exports.push(Export {
                file_system_id: file_system.file_system_id.clone(),
                addresses,
                files,
            });
        }
    }
    Some(Box::new(S3FilesClient { exports }))
}

impl EnvironmentPrograms for S3FilesClient {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        // Which program the command names, and whether its helper is one of this data plane's,
        // is the shared reading's
        // ([`::service_common::posix_shell_mounts::is_mount_program`]).
        if is_mount_program(name, SERVED_MOUNT_TYPES) {
            return self.mounted(arguments, files);
        }
        if is_unmount_program(name, SERVED_MOUNT_TYPES) {
            return self.unmounted(arguments, files);
        }
        None
    }
}

impl S3FilesClient {
    /// Perform `mount`: attach the file system the source names at the directory the target
    /// names, so every path under it is a path of that file system from then on.
    ///
    /// The command line is read by the shared reading of a `mount(8)` invocation
    /// ([`::service_common::posix_shell_mounts::mount_invocation`], which states which
    /// invocations are performed at all), narrowed to the types this data plane serves. `None`
    /// for an invocation it performs nothing of, and for one whose source names NOTHING of
    /// this service while the command named no type of its own — another filesystem's mount is
    /// not this program's to fail. Otherwise the status `mount` exits with: success for a
    /// mount that was made, and [`MOUNT_FAILURE`] for one that was not — nothing of this
    /// service answers where this machine stands, the file system holds no such path, or the
    /// machine has no directory to mount it over.
    fn mounted(
        &self,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let invocation = mount_invocation(arguments, SERVED_MOUNT_TYPES)?;
        let (named, export_path) = mount_source(&invocation.source)?;
        let failed = Some(ProgramOutcome {
            status: MOUNT_FAILURE,
            output: Vec::new(),
        });
        let Some(export) = self.export_named(&named) else {
            // A command that named THIS service's filesystem type meant this file system, so
            // a source nothing of it answers is a mount that failed; one that named no type
            // at all may have meant another filesystem, which this program performs nothing
            // of.
            return match named_this_file_system(invocation.file_system_type.as_deref()) {
                true => failed,
                false => None,
            };
        };
        let facts = MountFacts {
            file_system_type: MOUNTED_FILE_SYSTEM_TYPE.to_string(),
            statfs_type: MOUNTED_STATFS_TYPE.to_string(),
            source: invocation.source.clone(),
            options: invocation.mount_options(),
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

    /// Perform `umount`: take the file system mounted at the target away again, leaving the
    /// directory it was mounted over as it was.
    ///
    /// `None` for an invocation this program performs nothing of, and for a path holding a
    /// mount that is not one of THIS service's: another filesystem's unmount belongs to the
    /// program that mounted it. Otherwise success for a file system that was mounted there,
    /// and [`MOUNT_FAILURE`] for a target nothing of this service was — which is what
    /// `umount` answers for a path that is not a mount point.
    fn unmounted(
        &self,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let invocation = unmount_invocation(arguments, SERVED_MOUNT_TYPES)?;
        let mounted = files.mounted_at(&invocation.target);
        let ours = mounted.as_ref().is_some_and(|facts| {
            mount_source(&facts.source)
                .is_some_and(|(named, _)| self.export_named(&named).is_some())
        });
        if !ours {
            // A path holding ANOTHER filesystem's mount is that program's to unmount; one
            // holding nothing at all is this program's failure only where the command named
            // this service's own filesystem type.
            if mounted.is_some() || !named_this_file_system(invocation.file_system_type.as_deref())
            {
                return None;
            }
            return Some(ProgramOutcome {
                status: MOUNT_FAILURE,
                output: Vec::new(),
            });
        }
        let status = if files.unmount(&invocation.target) {
            SUCCESS
        } else {
            MOUNT_FAILURE
        };
        Some(ProgramOutcome {
            status,
            output: Vec::new(),
        })
    }

    /// The export `named` names, and `None` for a name nothing this client can reach answers
    /// to.
    ///
    /// Three ways name one endpoint, all of which a documented mount is written with: the
    /// FILE SYSTEM's own id, which is what the service's own mount helper takes; one of the
    /// ADDRESSES the mount target reports, which is how any NFS mount names its server; and a
    /// HOST NAME whose first label is the file system's id, which is how a name resolving to
    /// the mount target of the client's own zone is written.
    fn export_named(&self, named: &str) -> Option<&Export> {
        let first_label = named.split('.').next().unwrap_or(named);
        self.exports.iter().find(|export| {
            export.file_system_id == named
                || export.file_system_id == first_label
                || export.addresses.iter().any(|address| address == named)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{named_this_file_system, S3FILES_MOUNT_TYPE};

    /// RULE: only the service's OWN filesystem type makes a mount one this data plane has to
    /// answer for; every other type a `-t` may name — and a command that named none — leaves
    /// the invocation one another data plane may perform.
    #[test]
    fn only_this_services_own_type_claims_a_mount() {
        // (case, the type the command named, whether the mount is this service's to fail)
        let rows = [
            ("the service's own type", Some(S3FILES_MOUNT_TYPE), true),
            ("the type the kernel holds the mount as", Some("nfs4"), false),
            ("the other spelling of it", Some("nfs"), false),
            ("no type at all", None, false),
        ];
        for (case, named, expected) in rows {
            let claimed = named_this_file_system(named);
            assert_eq!(
                claimed, expected,
                "case {case:?}: expected a mount of {named:?} to be claimed = {expected}, got \
                 {claimed}"
            );
        }
    }
}
