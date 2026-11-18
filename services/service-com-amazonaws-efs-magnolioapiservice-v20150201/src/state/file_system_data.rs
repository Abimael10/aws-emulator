// SPDX-License-Identifier: CC0-1.0

//! What a file system HOLDS: the POSIX tree a client that has mounted it reads and writes.
//!
//! # The rule this module implements
//!
//! An Amazon EFS file system is "a file system for use with Amazon Web Services cloud
//! services": what makes it one is that a client which mounts it reads and writes files in
//! it, and that every client mounting the same file system sees the SAME files — "thousands
//! of Amazon EC2 instances can access the file system at the same time". So the file data is
//! the file system's own state, held here beside the record that names it, and a mount is a
//! handle onto that one tree rather than a copy of it: what one client writes the next client
//! to mount it reads.
//!
//! The API this service models carries no operation that reads or writes a file — the data
//! plane is the NFS mount, not this API — so nothing here is reported in a response. What
//! reaches the tree is a client that mounted the file system: the container of an Amazon ECS
//! task whose task definition declares the file system as a volume
//! (`…ecs…::state::container_volumes`), which takes the handle below and performs its
//! container's program over it.
//!
//! An ACCESS POINT is "an application-specific view into an EFS file system": it "applies an
//! operating system user and group, and a file system path, to any file system request made
//! through the access point", so a client mounting through one has that path as its root
//! rather than the file system's own ([`access_point_root`]) and acts as the access point's
//! `PosixUser` where it names one ([`access_point_owner`]). Both are read off the stored
//! access point, so the view a client is given is the one this service's own
//! `DescribeAccessPoints` reports.
//!
//! # Bounds
//!
//! The tree is in memory and bounded by [`FILE_SYSTEM_LIMITS`]: how many files and
//! directories it holds and how many bytes they come to. A write past one does not happen and
//! is reported to the writing client as the failure it is. No host path is opened, created or
//! read: every entry is one this process holds.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ::service_common::posix_shell_volume::{ShellVolume, VolumeLimits};

use super::*;

/// How much ONE file system holds. Every field is the EMULATOR'S number, not a quota of the
/// service — an Amazon EFS file system "grows and shrinks automatically as you add and remove
/// files" — because the tree is this process's memory: far above what a scenario stages
/// through a file system and far below what would cost this process anything noticeable.
pub const FILE_SYSTEM_LIMITS: VolumeLimits = VolumeLimits {
    files: 1024,
    bytes: 4 * 1024 * 1024,
    path_bytes: 4096,
    directories: 1024,
    device_lines: 0,
    device_line_bytes: 0,
};

/// A handle onto the files ONE file system holds, shared by every client that has mounted it:
/// what one writes through its handle, another reads through its own.
#[derive(Debug, Clone)]
pub struct MountedFileSystem {
    /// The one tree the file system holds.
    files: Arc<Mutex<ShellVolume>>,
}

impl MountedFileSystem {
    /// An empty file system — one nothing has written to yet.
    fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(ShellVolume::new(FILE_SYSTEM_LIMITS))),
        }
    }

    /// The tree, with a lock poisoned by an earlier panic taken rather than propagated: a
    /// file system outlives the client that was writing to it, so one failed write does not
    /// take the file system's data with it.
    pub fn files(&self) -> MutexGuard<'_, ShellVolume> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ComAmazonawsEfsMagnolioapiserviceV20150201State {
    /// A handle onto the files the file system `file_system_id` holds, for a client that has
    /// MOUNTED it — and `None` when this store holds no such file system, which is a mount
    /// of nothing.
    ///
    /// The handle is MINTED on the first mount and kept with the store from then on, so the
    /// second client to mount the same file system is handed the same tree. Handles whose
    /// file system is gone are dropped as one is handed out, so a deleted file system takes
    /// its data with it and this map is bounded by the file systems the store holds.
    pub fn mounted_file_system(&mut self, file_system_id: &str) -> Option<MountedFileSystem> {
        if !self.file_systems.contains_key(file_system_id) {
            return None;
        }
        let held = &self.file_systems;
        self.file_system_data
            .retain(|id, _| held.contains_key(id.as_str()));
        Some(
            self.file_system_data
                .entry(file_system_id.to_string())
                .or_insert_with(MountedFileSystem::new)
                .clone(),
        )
    }
}

/// A handle onto the files `file_system_id` holds, read out of THIS service's own store
/// through the one shared engine — so a client that mounts a file system reaches the data of
/// the file system this service holds, with no second copy of it kept anywhere else.
///
/// `None` when that store is unreachable (the engine is not the shared in-memory one) or
/// holds no such file system: a mount of a file system that does not exist reaches nothing,
/// and nothing is invented for it.
///
/// LOCK ORDER: this takes THIS store's lock, holds no other while it does, and hands back an
/// owned handle rather than a guard, so a caller may call it with no guard of its own store
/// held.
pub fn mount_file_system<Eng: Send + Sync + 'static>(
    engine: &Eng,
    file_system_id: &str,
) -> Option<MountedFileSystem> {
    let state = service_state(engine)?;
    // Bound to its own statement, so this store's lock is released before the handle is
    // handed back rather than held across the return.
    let mounted = state.lock().mounted_file_system(file_system_id);
    mounted
}

/// The path an access point enforces as the root of every request made through it —
/// `RootDirectory$Path`, "the path on the EFS file system to expose as the root directory to
/// NFS clients using the access point to access the EFS file system" — and `None` when this
/// account holds no such access point, or when that access point belongs to another file
/// system than `file_system_id`.
///
/// The root defaults to the file system's own root, which is what an access point created
/// with no `RootDirectory` exposes.
///
/// LOCK ORDER: as [`mount_file_system`].
pub fn access_point_root<Eng: Send + Sync + 'static>(
    engine: &Eng,
    file_system_id: &str,
    access_point_id: &str,
) -> Option<String> {
    let state = service_state(engine)?;
    let store = state.lock();
    let access_point = store.access_points.get(access_point_id)?;
    if access_point.file_system_id != file_system_id {
        return None;
    }
    Some(
        access_point
            .root_directory
            .path
            .clone()
            .unwrap_or_else(|| ROOT_DIRECTORY.to_string()),
    )
}

/// The operating-system user an access point applies to every request made through it —
/// `PosixUser`, "the full POSIX identity, including the user ID, group ID, and any secondary
/// group IDs, on the access point that is used for all file system operations performed by
/// NFS clients using the access point" — as `(uid, gid)`, and `None` for an access point
/// that enforces none (the client's own identity is then what reaches the file system).
///
/// LOCK ORDER: as [`mount_file_system`].
pub fn access_point_owner<Eng: Send + Sync + 'static>(
    engine: &Eng,
    access_point_id: &str,
) -> Option<(i64, i64)> {
    let state = service_state(engine)?;
    let store = state.lock();
    let posix_user = store
        .access_points
        .get(access_point_id)?
        .posix_user
        .as_ref()?;
    Some((posix_user.uid, posix_user.gid))
}

/// The root of a file system, which is the path a mount that names no directory is rooted at:
/// "if this parameter is omitted, the root of the Amazon EFS volume will be used"
/// (`EFSVolumeConfiguration$rootDirectory`).
pub const ROOT_DIRECTORY: &str = "/";
