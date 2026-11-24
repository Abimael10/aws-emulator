// SPDX-License-Identifier: CC0-1.0

//! The FILE SYSTEMS one invocation's execution environment really mounts: an entry of the
//! function's `FileSystemConfigs` resolved to the Amazon EFS access point it names, the files
//! behind that access point, and what the function's execution role may do to them.
//!
//! # The rule this module implements
//!
//! "Connection settings for an Amazon EFS file system" (`FileSystemConfigs`) names an access
//! point and the path it is mounted at — `FileSystemConfig$LocalMountPath`, "the path where the
//! function can access the file system, starting with /mnt/"
//! (`smithy_query(query="shape",shape="FileSystemConfig")`) — so the execution environment of
//! every invocation of that function has the access point's own root directory MOUNTED at that
//! path: a handler reads and writes files there, and the files are the FILE SYSTEM's, held by
//! the service that owns it
//! ([`::service_com_amazonaws_efs_magnolioapiservice_v20150201::state::mount_access_point`]).
//! That is the whole of what mounting one is for, so:
//!
//! * two functions that mount the same file system read and write ONE namespace — what one
//!   writes, the other reads — and what an invocation wrote is there for the next invocation,
//!   because the tree is the file system's and is never copied;
//! * an ACCESS POINT is "an application-specific view into an EFS file system": it "applies an
//!   operating system user and group, and a file system path, to any file system request made
//!   through the access point", so the mount is rooted at `RootDirectory$Path` — which confines
//!   the function to that subtree — and the identity every request through it is made as is the
//!   access point's `PosixUser`;
//! * what the function may DO there follows from its execution role: an NFS client writes to
//!   an Amazon EFS file system under `elasticfilesystem:ClientWrite` (Amazon EFS User Guide,
//!   "Using IAM to control file system data access"), so a function whose role may not make
//!   that call has the file system mounted READ-ONLY and a write under the mount point is
//!   refused where it is made.
//!
//! The entry is resolved per INVOCATION, so a mount is backed by the access point and file
//! system Amazon EFS holds at the instant the handler runs: a configuration naming an access
//! point this emulator's Amazon EFS does not hold — or holds as one still `creating` — mounts
//! NOTHING at that path, rather than a path backed by state nothing holds. A function's own
//! `FileSystemConfigs` cannot name one without a `VpcConfig` at all
//! ([`super::file_system_attachment`]), which is the other half of reaching a file system.
//!
//! # Where this happens
//!
//! Resolving a mount READS another service's store, so it happens where the rest of an
//! invocation is assembled, and the handles travel to the run's own thread with the invocation
//! ([`super::execution::Invocation`]).
//!
//! LOCK ORDER: this takes the Amazon EFS store's lock and then the IAM store's, each holding no
//! other while it does, and hands back owned handles rather than a guard — so it is called
//! either with no guard at all, or (as the queued-event delivery of `Invoke` does, beside the
//! SDK seam and the log target it is assembled with) under this service's own guard, which is
//! taken ahead of both.

use std::time::SystemTime;

use ::service_com_amazonaws_efs_magnolioapiservice_v20150201::state as efs;
use ::service_common::posix_shell_volume::resolved_path;

use super::event_source_authorization::iam_state;
use super::FunctionConfig;

/// The IAM action namespace the calls an NFS client makes on a file system are named in, which
/// is Amazon EFS's own.
const ELASTIC_FILE_SYSTEM_NAMESPACE: &str = "elasticfilesystem";

/// The call a client makes to WRITE to one, which is what a function short of it has the file
/// system read-only under.
const CLIENT_WRITE_ACTION: &str = "ClientWrite";

/// ONE file system an invocation's execution environment mounts: where it is mounted, the view
/// the access point enforces, what the function may do there, and the files themselves.
#[derive(Debug, Clone)]
pub struct FileSystemMount {
    /// `FileSystemConfig$LocalMountPath`, resolved: the path inside the execution environment
    /// the access point's root directory is mounted at.
    pub local_mount_path: String,
    /// The file system reached through the mount.
    pub file_system_id: String,
    /// The access point the configuration named.
    pub access_point_id: String,
    /// `RootDirectory$Path` of that access point, which is what the mount point shows.
    pub root_directory: String,
    /// The user every request through the access point is made as, and `None` for an access
    /// point that enforces no identity.
    pub uid: Option<i64>,
    /// The group beside it, read the same way.
    pub gid: Option<i64>,
    /// `PosixUser$SecondaryGids`, "secondary POSIX group IDs used for all file system
    /// operations using this access point".
    pub secondary_gids: Vec<i64>,
    /// Who the access point's ROOT DIRECTORY belongs to, and the permissions it carries, as
    /// `(uid, gid, mode)` — `RootDirectory$CreationInfo`, "the POSIX IDs and permissions to
    /// apply to the access point's `RootDirectory`", which Amazon EFS creates that directory
    /// with. `None` for an access point that states none.
    pub root_creation: Option<(i64, i64, u32)>,
    /// Whether the function's execution role may WRITE to the file system: a role short of
    /// `elasticfilesystem:ClientWrite` has it mounted read-only.
    pub writable: bool,
    /// The DEVICE the filesystem at this mount reports itself as, which is what tells a
    /// program that the mount point is a mount of its own rather than part of the directory
    /// above it. Derived from the file system's own id, so every reading of one mount reports
    /// one device and two file systems never report the same.
    pub device: u64,
    /// The file system's own files, shared with every other client that mounted it.
    pub files: efs::MountedFileSystem,
}

/// The file systems an invocation of a function configured by `config` mounts, in the order
/// the configuration names them.
///
/// An entry that resolves to no access point of this emulator's Amazon EFS, and one whose
/// execution role may not mount the file system at all, mounts NOTHING and is left out: the
/// path is then not a path of the execution environment, exactly as it is for a function that
/// configures no file system. See the module docs for the lock order this is called under.
#[must_use]
pub fn mounted_file_systems<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    config: &FunctionConfig,
    now: SystemTime,
) -> Vec<FileSystemMount> {
    let mut mounted = Vec::new();
    for requested in &config.file_system_configs {
        // The member admits the access point of an Amazon S3 file system too, which is reached
        // over that service's own endpoint rather than as an NFS mount of this one
        // ([`super::file_system_attachment`]): this module mounts the Amazon EFS ones.
        if ::service_common::arn::vendor(&requested.arn) != Some(ELASTIC_FILE_SYSTEM_NAMESPACE) {
            continue;
        }
        let Some(local_mount_path) = resolved_path(&requested.local_mount_path) else {
            continue;
        };
        let Some(access_point) = efs::mount_access_point(engine, &requested.arn, now) else {
            ::tracing::warn!(
                access_point = %requested.arn,
                "this emulator's Amazon EFS holds no available access point under that ARN, so \
                 nothing is mounted at the function's local mount path"
            );
            continue;
        };
        let writable = may_write(engine, region, &config.role, &access_point.file_system_id);
        let (uid, gid) = match access_point.posix_user {
            Some((uid, gid)) => (Some(uid), Some(gid)),
            None => (None, None),
        };
        mounted.push(FileSystemMount {
            local_mount_path,
            device: device_of(&access_point.file_system_id),
            file_system_id: access_point.file_system_id.clone(),
            access_point_id: access_point.access_point_id.clone(),
            root_directory: access_point.root_directory.clone(),
            uid,
            gid,
            secondary_gids: access_point.secondary_gids.clone(),
            root_creation: access_point.root_creation.as_ref().map(
                |(uid, gid, permissions)| (*uid, *gid, mode_of(permissions)),
            ),
            writable,
            files: access_point.files,
        });
    }
    mounted
}

/// The permissions `written` states, as the mode bits a filesystem reading reports:
/// `CreationInfo$Permissions` is "a POSIX permissions value" written in octal ("for example,
/// 0755"), which is how a `chmod` names one.
fn mode_of(written: &str) -> u32 {
    u32::from_str_radix(written.trim(), 8).unwrap_or(0) & 0o7777
}

/// Whether the execution role `role_arn` may WRITE to `file_system_id`, as the policies in
/// force on that role say.
///
/// Nothing is withheld on evidence this emulator does not have, which is the reading this crate
/// states for every question about an execution role ([`super::role_actions`]): an engine
/// carrying no IAM store, and a role ARN that store holds no role for, show nothing about the
/// role, so the file system is writable.
fn may_write<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    role_arn: &str,
    file_system_id: &str,
) -> bool {
    let Some(iam) = iam_state(engine) else {
        return true;
    };
    let withheld = super::event_source_authorization::withholds_call(
        &iam.lock(),
        role_arn,
        &format!("{ELASTIC_FILE_SYSTEM_NAMESPACE}:{CLIENT_WRITE_ACTION}"),
        &file_system_resource(region, file_system_id),
    );
    !withheld.unwrap_or(false)
}

/// The resource those calls are made on: the file system itself, in this account and the Region
/// the function is configured in, which is how a policy scoped to one file system names it.
fn file_system_resource(region: &str, file_system_id: &str) -> String {
    ::engine::emulator_arn(
        ELASTIC_FILE_SYSTEM_NAMESPACE,
        region,
        &format!("file-system/{file_system_id}"),
    )
}

/// The device number the filesystem of the file system `file_system_id` reports.
///
/// A number of this emulator's own — a device number is the operating system's, not anything
/// AWS publishes — derived from the file system's id so that it is the same number at every
/// reading of one mount and a different one for another file system, which is what a program
/// asking whether a path is a mount point compares. Never zero: that is the device the
/// environment's own storage reports.
fn device_of(file_system_id: &str) -> u64 {
    // FNV-1a over the id, which is a hash of its bytes and nothing more.
    let mut hashed: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in file_system_id.as_bytes() {
        hashed ^= u64::from(*byte);
        hashed = hashed.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Held well inside what a `st_dev` carries, and away from zero.
    (hashed % 0x0000_ffff_ffff_ffff) + 1
}

#[cfg(test)]
mod tests {
    use super::{device_of, file_system_resource};

    /// The resource those calls are made on is the file system itself, in the Region the
    /// function is configured in and this emulator's own account.
    #[test]
    fn the_resource_is_the_file_system_in_the_functions_region() {
        for (region, file_system_id) in [
            ("us-east-1", "fs-0123456789abcdef0"),
            ("eu-west-2", "fs-00000000000000001"),
        ] {
            let resource = file_system_resource(region, file_system_id);
            let expected = format!(
                "arn:aws:elasticfilesystem:{region}:{}:file-system/{file_system_id}",
                ::engine::emulator_account_id()
            );
            assert_eq!(
                resource, expected,
                "case {file_system_id:?}: expected {expected:?}, got {resource:?}"
            );
        }
    }

    /// RULE: one file system's mount reports one device, two file systems never report the
    /// same, and no file system reports the device of the environment's own storage.
    #[test]
    fn a_file_system_reports_its_own_device() {
        let ids = [
            "fs-0123456789abcdef0",
            "fs-0123456789abcdef1",
            "fs-fedcba98765432100",
            "fs-00000000000000000",
        ];
        for id in ids {
            let device = device_of(id);
            assert_eq!(
                device,
                device_of(id),
                "case {id:?}: two readings of one file system must report one device"
            );
            assert_ne!(
                device, 0,
                "case {id:?}: a mounted file system is not the environment's own storage"
            );
        }
        for (first, second) in ids.iter().zip(ids.iter().skip(1)) {
            assert_ne!(
                device_of(first),
                device_of(second),
                "case ({first:?}, {second:?}): two file systems must report two devices"
            );
        }
    }
}
