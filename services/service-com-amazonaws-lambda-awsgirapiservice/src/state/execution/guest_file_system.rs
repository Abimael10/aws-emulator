// SPDX-License-Identifier: CC0-1.0

//! The NETWORK FILE SYSTEMS a `python3.x` execution environment mounts: the Amazon EFS access
//! points the function's `FileSystemConfigs` name, served at their own `LocalMountPath`.
//!
//! # The rule this module implements
//!
//! `FileSystemConfig$LocalMountPath` is "the path where the function can access the file
//! system, starting with /mnt/", so the access point's root directory is MOUNTED there inside
//! every execution environment of that function: the mount point is a mount of its own (a
//! program asking whether it is one is told it is), the files under it are the FILE SYSTEM's —
//! held by the service that owns it, never copied ([`super::super::file_system_mounts`]) — and
//! the handler reads and writes them with ordinary filesystem calls. So a file one function
//! writes under its mount path is a file another function that mounts the same file system
//! reads under its own, and one an invocation wrote is there for the next invocation.
//!
//! What the mount lets the handler DO is the access point's and the execution role's:
//!
//! * the identity every request through an access point is made as is its `PosixUser`, so the
//!   handler's own process reports that user and group, and the entries it creates belong to
//!   them. An environment whose function mounts no access point that enforces one runs as the
//!   user a function runs as otherwise, which is the same user the filesystem of the
//!   environment already reports its files are owned by ([`super::guest_ephemeral`]);
//! * a function whose execution role may not write to the file system has it mounted READ-ONLY,
//!   so a write under the mount point answers `EROFS` the way a write to a read-only NFS mount
//!   does, and the handler that catches it reads that errno.
//!
//! # What it reaches
//!
//! Nothing of the host: the files are the ones this process holds for the file system
//! ([`::service_common::posix_shell_volume::ShellVolume`], the one tree every client of that
//! file system shares), and the calls below are answered out of it. The path a call names is
//! resolved first and then looked up in the MOUNTS of this function, so:
//!
//! * a path on none of them is answered by the layer below this one — the ephemeral storage,
//!   the read-only mounts, and that module's own refusal for a path on neither
//!   ([`super::guest_mount`]) — so this module widens what the environment has and takes
//!   nothing away;
//! * a path under a mount point cannot climb OUT of the access point's root directory: `..`
//!   above a mount's root resolves to that root, which is exactly the confinement an access
//!   point enforces;
//! * every write is bounded by the file system's own bounds, held by the service that owns it,
//!   and a write past one is refused as a full filesystem refuses it.
//!
//! The guest reaches this through ONE host function (`_lambda_host.files`), which takes a
//! document naming the call and the path and answers a document: no file object, no descriptor
//! and no path of the host crosses it, and the bounds of the run are not reachable from it.

use ::serde_json::{json, Value};
use ::service_com_amazonaws_efs_magnolioapiservice_v20150201::state as efs;
use ::service_common::posix_shell_volume::{
    parent_path, resolved_path, EntryKind, ShellVolume,
};

use crate::state::FileSystemMount;

/// The size of a block this filesystem reports, which is also the longest path an Amazon EFS
/// file system accepts.
const BLOCK_SIZE: usize = 4096;

/// Who an entry of a mount belongs to when the access point enforces no identity: the user and
/// group a Lambda execution environment runs as, which is the owner the environment's own
/// filesystem reports for the files a function writes ([`super::guest_ephemeral`], whose own
/// source states the same two numbers for the same reason).
const SANDBOX_UID: i64 = 993;
const SANDBOX_GID: i64 = 990;

/// The mode an entry of a mount carries when the file system holds none for it: the
/// permissions a process with the usual mask leaves on what it creates.
const DEFAULT_FILE_MODE: u32 = 0o644;
const DEFAULT_DIRECTORY_MODE: u32 = 0o755;

/// The file systems ONE execution environment has mounted, as the host serves them.
///
/// A path the guest names is looked up in the MOUNTS of this function and then in the file
/// system that holds it — the one tree every client of that file system shares, reached through
/// its own lock for the one call and not held across two — so what this answers is the file
/// system's own content and never a copy of it.
#[derive(Debug)]
pub(super) struct GuestFileSystems {
    /// What each mount is, DEEPEST mount point first, so a path lands in the deepest mount
    /// that holds it.
    mounts: Vec<Attached>,
}

/// ONE mounted file system, as this environment holds it.
#[derive(Debug)]
struct Attached {
    /// The resolved `LocalMountPath`.
    path: String,
    /// The path INSIDE the file system that the mount point shows: the access point's own
    /// `RootDirectory`, resolved.
    root: String,
    /// The access point's `PosixUser`, when it enforces one.
    uid: Option<i64>,
    gid: Option<i64>,
    secondary_gids: Vec<i64>,
    /// Whether the function's execution role may write to the file system.
    writable: bool,
    /// The device this filesystem reports itself as.
    device: u64,
    /// The file system's own files, shared with every other client that mounted it.
    files: efs::MountedFileSystem,
}

impl Attached {
    /// Whether the resolved `path` lies in this mount — the mount point itself, or anything
    /// under it. Only a SEPARATOR after the mount point puts a path inside it: `/mnt/datax` is
    /// not inside the mount at `/mnt/data`.
    fn holds(&self, path: &str) -> bool {
        match path.strip_prefix(self.path.as_str()) {
            Some("") => true,
            Some(under) => under.starts_with('/'),
            None => false,
        }
    }

    /// The path INSIDE the file system that the resolved `path` names — under the access
    /// point's own root directory, which is what confines the function to it.
    fn inside(&self, path: &str) -> String {
        let relative = path
            .strip_prefix(self.path.as_str())
            .unwrap_or_default()
            .trim_start_matches('/');
        if relative.is_empty() {
            return self.root.clone();
        }
        if self.root.ends_with('/') {
            return format!("{}{relative}", self.root);
        }
        format!("{}/{relative}", self.root)
    }
}

impl GuestFileSystems {
    /// The environment that has mounted nothing, which is what a function configuring no file
    /// system has.
    pub(super) fn none() -> Self {
        Self {
            mounts: Vec::new(),
        }
    }

    /// Mount each of `mounts` at its own `LocalMountPath`.
    pub(super) fn attach(mounts: &[FileSystemMount]) -> Self {
        let mut attached = Self::none();
        for mount in mounts {
            attached.mount(mount);
        }
        // Deepest first: a path is matched against the deepest mount that holds it, which is
        // how a mount inside another mount's directory shadows it.
        attached
            .mounts
            .sort_by(|left, right| right.path.len().cmp(&left.path.len()).then_with(|| left.path.cmp(&right.path)));
        attached
    }

    /// Attach ONE file system, leaving this environment unchanged when it cannot be mounted.
    ///
    /// The access point's ROOT DIRECTORY is made as the mount is attached, with the identity and
    /// the permissions `RootDirectory$CreationInfo` states for it: "if the `RootDirectory` >
    /// `Path` specified does not exist, Amazon EFS creates it and applies the `CreationInfo`
    /// settings … If an access point already exists and the `RootDirectory` > `Path` does not
    /// exist, attempts to mount the file system using the access point will fail". So an access
    /// point whose path the file system does not hold and that states NO `CreationInfo` mounts
    /// nothing, and one whose directory the file system ALREADY holds is left exactly as it is:
    /// that directory is another client's, and the access point creates nothing over it. A file
    /// system that will not hold the directory mounts nothing either — the path is then not a
    /// path of the execution environment at all, rather than one backed by nothing.
    fn mount(&mut self, mount: &FileSystemMount) {
        let Some(root) = resolved_path(&mount.root_directory) else {
            return;
        };
        // Bound to its own statement, so the file system's own lock is taken for the creation
        // and released again rather than held.
        let rooted = {
            let mut files = mount.files.files();
            if files.is_directory(&root) {
                true
            } else {
                match mount.root_creation {
                    Some((uid, gid, permissions)) if files.make_directories(&root) => {
                        files.set_owner(&root, uid, gid);
                        files.set_mode(&root, permissions);
                        true
                    }
                    _ => false,
                }
            }
        };
        if !rooted {
            ::tracing::warn!(
                file_system = %mount.file_system_id,
                root = %mount.root_directory,
                "that file system holds no directory at the access point's root and the access \
                 point states no creation settings for one, so nothing is mounted at the \
                 function's local mount path"
            );
            return;
        }
        self.mounts.push(Attached {
            path: mount.local_mount_path.clone(),
            root,
            uid: mount.uid,
            gid: mount.gid,
            secondary_gids: mount.secondary_gids.clone(),
            writable: mount.writable,
            device: mount.device,
            files: mount.files.clone(),
        });
    }

    /// The mount table the GUEST is handed: where each file system is mounted, the identity
    /// requests through it are made as, whether it may be written and the device it reports —
    /// plus the directories ABOVE the mount points, which a mount makes visible (`/mnt` is a
    /// directory of the environment because something is mounted under it).
    pub(super) fn table(&self) -> Value {
        let mounted: Vec<Value> = self
            .mounts
            .iter()
            .map(|mount| {
                json!({
                    "path": mount.path,
                    "uid": mount.uid,
                    "gid": mount.gid,
                    "secondaryGids": mount.secondary_gids,
                    "writable": mount.writable,
                    "device": mount.device,
                })
            })
            .collect();
        json!({ "mounts": mounted, "above": self.above() })
    }

    /// The directories above the mount points, deepest first — every directory of the path a
    /// file system is mounted at, below the root itself, which the mount makes a directory of
    /// this environment.
    fn above(&self) -> Vec<String> {
        let mut above: Vec<String> = Vec::new();
        for mount in &self.mounts {
            let mut walked = String::new();
            let mut segments: Vec<&str> =
                mount.path.split('/').filter(|part| !part.is_empty()).collect();
            // The mount point itself is the mount, not a directory above it.
            segments.pop();
            for segment in segments {
                walked.push('/');
                walked.push_str(segment);
                if !above.contains(&walked) {
                    above.push(walked.clone());
                }
            }
        }
        above.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        above
    }

    /// What the host answers for ONE call the guest made on a path of these mounts.
    ///
    /// Every answer is a document: what the call found, or which refusal it came to — the
    /// errno the guest raises follows from that word, so no refusal of this layer is worded by
    /// the host.
    pub(super) fn answered(&mut self, document: &str) -> String {
        let Ok(request) = ::serde_json::from_str::<Value>(document) else {
            return refused("unreachable").to_string();
        };
        let operation = request["op"].as_str().unwrap_or_default().to_string();
        let Some(path) = resolved_path(request["path"].as_str().unwrap_or_default()) else {
            return refused("unreachable").to_string();
        };
        let answered = match operation.as_str() {
            "stat" => self.stat(&path),
            "list" => self.list(&path),
            "read" => self.read(
                &path,
                usize::try_from(request["from"].as_u64().unwrap_or_default()).unwrap_or(0),
                request["size"]
                    .as_u64()
                    .and_then(|size| usize::try_from(size).ok()),
            ),
            "write" => self.write(
                &path,
                &decoded(request["bytes"].as_str().unwrap_or_default()),
                request["append"].as_bool().unwrap_or(false),
            ),
            "chmod" => self.chmod(&path, mode_asked(&request)),
            // The size a truncate asks for is the one value on this seam a caller can scale
            // freely, so it is decided BEFORE anything is held for it: a size this host cannot
            // even name is refused as a full filesystem refuses one, exactly as a size past
            // what the file system has room for is ([`Self::truncate`]).
            "truncate" => match request["size"]
                .as_u64()
                .and_then(|size| usize::try_from(size).ok())
            {
                Some(size) => self.truncate(&path, size),
                None => refused("no_space"),
            },
            "mkdir" => self.mkdir(&path, mode_asked(&request)),
            "rmdir" => self.rmdir(&path),
            "remove" => self.remove(&path),
            "rename" => self.rename(&path, request["target"].as_str().unwrap_or_default()),
            "statvfs" => self.statvfs(&path),
            _ => refused("unreachable"),
        };
        answered.to_string()
    }

    /// The DEEPEST mount holding `path` — the mount point itself, or anything under it — and
    /// `None` for a path on none of them.
    fn mounted(&self, path: &str) -> Option<&Attached> {
        self.mounts.iter().find(|mount| mount.holds(path))
    }

    /// Read the FILE SYSTEM that holds `path`, at the path inside it the mount point shows —
    /// and `None` for a path on no mount of this function.
    ///
    /// LOCK ORDER: the file system's own lock is taken for the one call and released with it,
    /// so no two of these are ever held at once and nothing of this environment is held while
    /// one is.
    fn held<T>(&self, path: &str, read: impl FnOnce(&ShellVolume, &str) -> T) -> Option<T> {
        let mount = self.mounted(path)?;
        let inside = mount.inside(path);
        let files = mount.files.files();
        Some(read(&files, &inside))
    }

    /// [`Self::held`] for a change of that file system.
    fn held_mut<T>(
        &self,
        path: &str,
        write: impl FnOnce(&mut ShellVolume, &str) -> T,
    ) -> Option<T> {
        let mount = self.mounted(path)?;
        let inside = mount.inside(path);
        let mut files = mount.files.files();
        Some(write(&mut files, &inside))
    }

    /// What the file system holds at `path`, as a listing of it reports that entry.
    fn facts(&self, path: &str) -> Option<::service_common::posix_shell_volume::EntryFacts> {
        self.held(path, |files, inside| files.facts(inside)).flatten()
    }

    /// Whether the file system holds a DIRECTORY at `path`.
    fn is_directory(&self, path: &str) -> bool {
        self.held(path, |files, inside| files.is_directory(inside))
            .unwrap_or(false)
    }

    /// Whether this layer answers `path` at all: a path of one of the mounts, or a directory
    /// above one of them.
    fn governs(&self, path: &str) -> bool {
        self.mounted(path).is_some() || self.above().iter().any(|above| above == path)
    }

    /// Whether a WRITE at `path` may happen: only inside a mount whose file system this
    /// function's role may write. A path above the mount points is on the environment's own
    /// root filesystem, which is read-only.
    fn may_write(&self, path: &str) -> bool {
        self.mounted(path).is_some_and(|mount| mount.writable)
    }

    /// Who an entry CREATED at `path` belongs to: the identity the access point of its mount
    /// enforces, and the user a function runs as where it enforces none.
    fn creator_of(&self, path: &str) -> (i64, i64) {
        let mount = self.mounted(path);
        (
            mount.and_then(|mount| mount.uid).unwrap_or(SANDBOX_UID),
            mount.and_then(|mount| mount.gid).unwrap_or(SANDBOX_GID),
        )
    }

    /// What a `stat` of `path` finds.
    fn stat(&self, path: &str) -> Value {
        if self.mounted(path).is_none() {
            if self.above().iter().any(|above| above == path) {
                // A directory ABOVE the mount points: the environment's own root filesystem,
                // which is where a mount point lies. What it reports is the mount's own
                // reading — a directory is as old as what is mounted under it — rather than
                // anything of this host's clock.
                let under = format!("{path}/");
                let modified = self
                    .mounts
                    .iter()
                    .filter(|mount| mount.path.starts_with(under.as_str()))
                    .filter_map(|mount| self.facts(&mount.path))
                    .map(|facts| facts.modified)
                    .min()
                    .unwrap_or(::std::time::SystemTime::UNIX_EPOCH);
                return json!({ "entry": {
                    "kind": "directory",
                    "size": 0,
                    "modified": epoch_seconds(modified),
                    "device": 0,
                    "uid": SANDBOX_UID,
                    "gid": SANDBOX_GID,
                    "mode": DEFAULT_DIRECTORY_MODE,
                } });
            }
            return refused("unreachable");
        }
        let Some(facts) = self.facts(path) else {
            // A path the mounts hold nothing at: absent, which is not a refusal of the call.
            return json!({ "entry": Value::Null });
        };
        // WHO the entry belongs to and WHAT permissions it carries are the file system's own,
        // held with the entry — so a file one function wrote reports that function's own
        // identity and mode to every other client of the file system. An entry nobody recorded
        // them for belongs to the identity of the mount reading it.
        let (uid, gid) = self
            .held(path, |files, inside| files.owner_of(inside))
            .flatten()
            .unwrap_or_else(|| self.creator_of(path));
        let directory = facts.kind == EntryKind::Directory;
        let mode = self
            .held(path, |files, inside| files.mode_of(inside))
            .flatten()
            .unwrap_or(if directory {
                DEFAULT_DIRECTORY_MODE
            } else {
                DEFAULT_FILE_MODE
            });
        json!({ "entry": {
            "kind": if directory { "directory" } else { "file" },
            "size": facts.size,
            "modified": epoch_seconds(facts.modified),
            "device": self.mounted(path).map_or(0, |mount| mount.device),
            "uid": uid,
            "gid": gid,
            "mode": mode,
        } })
    }

    /// The names the directory at `path` holds.
    fn list(&self, path: &str) -> Value {
        if self.mounted(path).is_none() {
            if self.above().iter().any(|above| above == path) {
                // What lies directly under a directory above the mount points: the next
                // segment of every mount point under it.
                let under = format!("{path}/");
                let mut names: Vec<String> = Vec::new();
                for mount in &self.mounts {
                    let Some(relative) = mount.path.strip_prefix(under.as_str()) else {
                        continue;
                    };
                    let name = relative.split('/').next().unwrap_or(relative).to_string();
                    if !name.is_empty() && !names.contains(&name) {
                        names.push(name);
                    }
                }
                names.sort();
                return json!({ "names": names });
            }
            return refused("unreachable");
        }
        match self.facts(path) {
            None => refused("absent"),
            Some(facts) if facts.kind != EntryKind::Directory => refused("not_a_directory"),
            Some(_) => json!({
                "names": self
                    .held(path, |files, inside| files.entry_names(inside))
                    .unwrap_or_default(),
            }),
        }
    }

    /// The bytes the file at `path` holds from `from` on, at most `size` of them — the slice a
    /// read of that file answers, beside how many bytes the whole file holds, so a caller
    /// reading it through needs no second call to learn where its end is.
    fn read(&self, path: &str, from: usize, size: Option<usize>) -> Value {
        if self.mounted(path).is_none() {
            return if self.governs(path) {
                refused("is_a_directory")
            } else {
                refused("unreachable")
            };
        }
        match self.facts(path) {
            None => refused("absent"),
            Some(facts) if facts.kind == EntryKind::Directory => refused("is_a_directory"),
            Some(_) => match self.held(path, |files, inside| files.read_bytes(inside)).flatten() {
                Some(bytes) => {
                    let held = bytes.len();
                    let from = from.min(held);
                    let until = size.map_or(held, |size| from.saturating_add(size).min(held));
                    json!({ "bytes": encoded(&bytes[from..until]), "size": held })
                }
                None => refused("absent"),
            },
        }
    }

    /// Write `bytes` at `path`, replacing what is there or appending to it.
    fn write(&mut self, path: &str, bytes: &[u8], append: bool) -> Value {
        if !self.may_write(path) {
            return refused("read_only");
        }
        let mut created = false;
        match self.facts(path) {
            Some(facts) if facts.kind == EntryKind::Directory => return refused("is_a_directory"),
            Some(_) => {}
            // A filesystem will not create a file in a directory it does not have.
            None => {
                if parent_path(path).is_some_and(|parent| !self.is_directory(&parent)) {
                    return refused("absent");
                }
                created = true;
            }
        }
        let wrote = self
            .held_mut(path, |files, inside| {
                files.write_bytes(inside, bytes, append)
            })
            .unwrap_or(false);
        if !wrote {
            return refused("no_space");
        }
        if created {
            // A file BELONGS TO the identity that created it, and carries the permissions it
            // was created with; both travel with the file.
            let (uid, gid) = self.creator_of(path);
            self.held_mut(path, |files, inside| {
                files.set_owner(inside, uid, gid);
                files.set_mode(inside, DEFAULT_FILE_MODE);
            });
        }
        json!({ "ok": true })
    }

    /// Make the file at `path` exactly `size` bytes long, as `truncate` does: a file made
    /// longer reads as zero bytes where it grew.
    ///
    /// The size is judged against the ROOM the file system has for it BEFORE anything is held
    /// for it: a file cannot be grown past what one file system of this emulator holds
    /// ([`efs::FILE_SYSTEM_LIMITS`]) less what this one already holds, so a size past that is
    /// refused as a full filesystem refuses one rather than being materialised first.
    fn truncate(&mut self, path: &str, size: usize) -> Value {
        if !self.may_write(path) {
            return refused("read_only");
        }
        let Some(held) = self.held(path, |files, inside| files.read_bytes(inside)).flatten() else {
            return refused("absent");
        };
        if size > self.room_for(path, held.len()) {
            return refused("no_space");
        }
        let mut bytes = held;
        bytes.resize(size, 0);
        self.write(path, &bytes, false)
    }

    /// How many bytes the file at `path` may come to: what ONE file system of this emulator
    /// holds at most, less what this one holds beside that file's own `held` bytes.
    fn room_for(&self, path: &str, held: usize) -> usize {
        let stored = self.mounted(path).map_or(0, |mount| {
            usize::try_from(mount.files.metered_bytes()).unwrap_or(0)
        });
        efs::FILE_SYSTEM_LIMITS
            .bytes
            .saturating_sub(stored.saturating_sub(held))
    }

    /// Make a directory at `path`, with the permissions `mode` asks for.
    fn mkdir(&mut self, path: &str, mode: Option<u32>) -> Value {
        if !self.may_write(path) {
            return refused("read_only");
        }
        if self.facts(path).is_some() {
            return refused("exists");
        }
        if parent_path(path).is_some_and(|parent| !self.is_directory(&parent)) {
            return refused("absent");
        }
        let (uid, gid) = self.creator_of(path);
        let permissions = mode.unwrap_or(DEFAULT_DIRECTORY_MODE);
        let made = self
            .held_mut(path, |files, inside| {
                if !files.make_directories(inside) {
                    return false;
                }
                files.set_owner(inside, uid, gid);
                files.set_mode(inside, permissions);
                true
            })
            .unwrap_or(false);
        if made {
            json!({ "ok": true })
        } else {
            refused("no_space")
        }
    }

    /// Set the PERMISSIONS the entry at `path` carries, which is what a `chmod` of it does.
    fn chmod(&mut self, path: &str, mode: Option<u32>) -> Value {
        if !self.may_write(path) {
            return refused("read_only");
        }
        if self.facts(path).is_none() {
            return refused("absent");
        }
        let permissions = mode.unwrap_or_default();
        self.held_mut(path, |files, inside| files.set_mode(inside, permissions));
        json!({ "ok": true })
    }

    /// Remove the EMPTY directory at `path`.
    fn rmdir(&mut self, path: &str) -> Value {
        if !self.may_write(path) {
            return refused("read_only");
        }
        match self.facts(path) {
            None => return refused("absent"),
            Some(facts) if facts.kind != EntryKind::Directory => {
                return refused("not_a_directory")
            }
            Some(_) => {}
        }
        if self.mounted(path).is_some_and(|mount| mount.path == path) {
            // The mount point itself is where the file system is mounted, not an entry
            // anybody removes.
            return refused("busy");
        }
        if !self
            .held(path, |files, inside| files.entry_names(inside))
            .unwrap_or_default()
            .is_empty()
        {
            return refused("not_empty");
        }
        if self
            .held_mut(path, |files, inside| files.remove_tree(inside))
            .unwrap_or(false)
        {
            json!({ "ok": true })
        } else {
            refused("absent")
        }
    }

    /// Remove the file at `path`.
    fn remove(&mut self, path: &str) -> Value {
        if !self.may_write(path) {
            return refused("read_only");
        }
        match self.facts(path) {
            None => refused("absent"),
            Some(facts) if facts.kind == EntryKind::Directory => refused("is_a_directory"),
            Some(_) => {
                if self
                    .held_mut(path, |files, inside| files.remove_file(inside))
                    .unwrap_or(false)
                {
                    json!({ "ok": true })
                } else {
                    refused("absent")
                }
            }
        }
    }

    /// Move the entry at `path` to `target`, which is what a rename is: the name moves and
    /// what it held is unchanged.
    fn rename(&mut self, path: &str, target: &str) -> Value {
        let Some(target) = resolved_path(target) else {
            return refused("unreachable");
        };
        if !self.may_write(path) || !self.may_write(&target) {
            return refused("read_only");
        }
        // A rename happens inside ONE filesystem: a name does not move from one device to
        // another, which is what a program is told by `EXDEV`.
        let same = match (self.mounted(path), self.mounted(&target)) {
            (Some(from), Some(to)) => from.path == to.path,
            _ => false,
        };
        if !same {
            return refused("cross_device");
        }
        // A directory does not move INSIDE ITSELF: there would be no fixed point, which is
        // what a program is told by `EINVAL`.
        if target == path || target.starts_with(&format!("{path}/")) {
            return refused("invalid");
        }
        let Some(facts) = self.facts(path) else {
            return refused("absent");
        };
        if parent_path(&target).is_some_and(|parent| !self.is_directory(&parent)) {
            return refused("absent");
        }
        if self
            .facts(&target)
            .is_some_and(|held| held.kind == EntryKind::Directory)
        {
            return refused("is_a_directory");
        }
        if facts.kind == EntryKind::Directory {
            return self.renamed_tree(path, &target);
        }
        let Some(bytes) = self.held(path, |files, inside| files.read_bytes(inside)).flatten() else {
            return refused("absent");
        };
        let owner = self.held(path, |files, inside| files.owner_of(inside)).flatten();
        let mode = self.held(path, |files, inside| files.mode_of(inside)).flatten();
        if !self
            .held_mut(&target, |files, inside| {
                files.write_bytes(inside, &bytes, false)
            })
            .unwrap_or(false)
        {
            return refused("no_space");
        }
        // A rename moves a NAME: what the entry held, who it belongs to and the permissions it
        // carries are unchanged.
        let (uid, gid) = owner.unwrap_or_else(|| self.creator_of(&target));
        let permissions = mode.unwrap_or(DEFAULT_FILE_MODE);
        self.held_mut(&target, |files, inside| {
            files.set_owner(inside, uid, gid);
            files.set_mode(inside, permissions);
        });
        self.held_mut(path, |files, inside| files.remove_file(inside));
        json!({ "ok": true })
    }

    /// [`Self::rename`] for a DIRECTORY: every entry under it moves with it.
    ///
    /// The subtree is walked ITERATIVELY, over a worklist bounded by the entries the file
    /// system holds ([`efs::FILE_SYSTEM_LIMITS`]), so no tree shape decides how deep this
    /// goes.
    fn renamed_tree(&mut self, path: &str, target: &str) -> Value {
        // The directories of the subtree, PARENTS FIRST, and the files under them: each is
        // named relative to the directory being moved.
        let mut pending: Vec<String> = vec![String::new()];
        let mut directories: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        while let Some(relative) = pending.pop() {
            let from = under(path, &relative);
            for name in self
                .held(&from, |held, inside| held.entry_names(inside))
                .unwrap_or_default()
            {
                let child = under(&relative, &name);
                if self.is_directory(&under(path, &child)) {
                    directories.push(child.clone());
                    pending.push(child);
                } else {
                    files.push(child);
                }
            }
        }
        for relative in ::std::iter::once(String::new()).chain(directories) {
            let made = self.made_directory_like(&under(path, &relative), &under(target, &relative));
            if made["ok"] != json!(true) {
                return made;
            }
        }
        for relative in files {
            let moved = self.rename(&under(path, &relative), &under(target, &relative));
            if moved["ok"] != json!(true) {
                return moved;
            }
        }
        if self
            .held_mut(path, |held, inside| held.remove_tree(inside))
            .unwrap_or(false)
        {
            json!({ "ok": true })
        } else {
            refused("not_empty")
        }
    }

    /// Make a directory at `target` carrying what the directory at `path` carries: a rename
    /// moves a name, so the owner and the permissions are the ones the source had.
    fn made_directory_like(&mut self, path: &str, target: &str) -> Value {
        let owner = self.held(path, |held, inside| held.owner_of(inside)).flatten();
        let mode = self.held(path, |held, inside| held.mode_of(inside)).flatten();
        let (uid, gid) = owner.unwrap_or_else(|| self.creator_of(target));
        let permissions = mode.unwrap_or(DEFAULT_DIRECTORY_MODE);
        let made = self
            .held_mut(target, |held, inside| {
                if !held.make_directories(inside) {
                    return false;
                }
                held.set_owner(inside, uid, gid);
                held.set_mode(inside, permissions);
                true
            })
            .unwrap_or(false);
        if made {
            json!({ "ok": true })
        } else {
            refused("no_space")
        }
    }

    /// What a `statvfs` of a path on one of these mounts reports: the file system's own
    /// capacity as this emulator holds it, and how much of it is used.
    fn statvfs(&self, path: &str) -> Value {
        let Some(mount) = self.mounted(path) else {
            return refused("unreachable");
        };
        if self.facts(path).is_none() {
            return refused("absent");
        }
        let limits = efs::FILE_SYSTEM_LIMITS;
        let held = usize::try_from(mount.files.metered_bytes()).unwrap_or(0);
        // Bound to its own statement, so the file system's lock is released before the answer
        // is built.
        let entries = mount.files.files().stored_bytes().count();
        json!({
            "blockSize": BLOCK_SIZE,
            "blocks": limits.bytes / BLOCK_SIZE,
            "free": limits.bytes.saturating_sub(held) / BLOCK_SIZE,
            "files": limits.files,
            "freeFiles": limits.files.saturating_sub(entries),
            "readOnly": !mount.writable,
        })
    }
}

/// The refusal `named`, as the answer carries it.
fn refused(named: &str) -> Value {
    json!({ "refused": named })
}

/// The path `relative` names under `path` — `path` itself for a relative path naming nothing.
fn under(path: &str, relative: &str) -> String {
    if relative.is_empty() {
        return path.to_string();
    }
    if path.ends_with('/') {
        return format!("{path}{relative}");
    }
    format!("{path}/{relative}")
}

/// `bytes` as the answer carries a file's content: base64, which is how a JSON document
/// carries bytes that are no text at all — and a file of a filesystem is bytes.
fn encoded(bytes: &[u8]) -> String {
    ::base64::Engine::encode(&::base64::engine::general_purpose::STANDARD, bytes)
}

/// The bytes `encoded` carries, and none for a value that is no base64 at all.
fn decoded(encoded: &str) -> Vec<u8> {
    ::base64::Engine::decode(&::base64::engine::general_purpose::STANDARD, encoded)
        .unwrap_or_default()
}

/// The PERMISSIONS one call asks for, and `None` for a call that names none.
fn mode_asked(request: &Value) -> Option<u32> {
    request["mode"]
        .as_u64()
        .and_then(|mode| u32::try_from(mode).ok())
        .map(|mode| mode & 0o7777)
}

/// `at` as the seconds since the epoch a filesystem reading reports.
fn epoch_seconds(at: ::std::time::SystemTime) -> f64 {
    at.duration_since(::std::time::SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// The Python source of this layer, which is the THIRD section of the runtime source: it is
/// evaluated in the runtime's own scope, after the read-only mounts and the ephemeral storage
/// ([`super::guest_mount`], [`super::guest_ephemeral`]) and before the runtime itself, which
/// installs all three.
///
/// A macro rather than a constant for the reason those two are: the sections are ONE compiled
/// unit — the runtime's own scope — so this section wraps the calls the ephemeral storage left
/// on `os` and hands a path on no mount of this function straight back to them, and the runtime
/// below calls `_efs_install`, which builds the mounts and puts the dispatching calls on `os`.
/// What it is handed is the rest of that source, as a literal.
macro_rules! file_system_source {
    ($($runtime:tt)*) => {
        concat!(
            r##"
"""The NETWORK FILE SYSTEMS of this execution environment: the access points it mounts."""

import base64
import builtins
import errno
import io
import json
import os

# The mounts of this environment, as the host stated them.
_EFS_MOUNTS = []
# The directories ABOVE those mount points, which a mount makes visible.
_EFS_ABOVE = []
# The calls installed BELOW this layer, which a path on no mount of this function is answered
# by: the ephemeral storage, the read-only mounts, and that layer's own refusal.
_EFS_BELOW = {}
# The descriptors open on a file of one of these mounts: the fd a caller holds, to the path it
# is open on, how far into the file it is and what it may do.
_EFS_DESCRIPTORS = {}
# The number a path's own file is identified by, handed out as paths are asked about, so two
# readings of one path report one file and two paths never report the same.
_EFS_INODES = {}
# The permissions and the times a program SET on one of these entries: a time is the
# environment's own reading (the file system records the instant of the write it made), and a
# mode travels WITH the entry, so what a `chmod` here sets is what every client of the file
# system reads afterwards.
_EFS_TIMES = {}
# Who a function's own files belong to when no access point narrows it, which is the user a
# function's process runs as and the user the filesystem of this environment reports its
# entries are owned by.
_EFS_SANDBOX_UID = 993
_EFS_SANDBOX_GID = 990
# The identity the handler's process runs as: an access point's `PosixUser` where one of the
# mounts enforces one, and the user above where none does.
_EFS_IDENTITY = {"uid": _EFS_SANDBOX_UID, "gid": _EFS_SANDBOX_GID, "groups": []}
# The mode an entry of one of these mounts reports when the file system holds none for it: the
# type bits of what it is, then the permissions a process with the usual mask leaves on what it
# creates. The MASK is what a process creating an entry takes off the mode it asked for.
_EFS_FILE_MODE = 0o100644
_EFS_DIRECTORY_MODE = 0o040755
_EFS_MASK = 0o022
_EFS_TYPE_MASK = 0o170000
# Said of a write to a filesystem mounted read-only, as the operating system says it.
_EFS_READ_ONLY = "Read-only file system"
# The longest a path component of this filesystem may be, which is what it reports as its
# `f_namemax`.
_EFS_MAX_NAME = 255
# How many bytes a stream asks the file system for while it looks for the end of a LINE: a
# line is however far the next break is, so a chunk is read rather than the whole file.
_EFS_LINE_CHUNK = 8192
# The calls this layer answers for a path of its mounts, each handed back to the layer below
# for every other path.
_EFS_SERVED = (
    "stat", "lstat", "listdir", "scandir", "access", "open", "read", "write", "close",
    "lseek", "fstat", "fstatvfs", "statvfs", "ftruncate", "truncate", "fsync", "fdatasync",
    "mkdir", "rmdir", "unlink", "remove", "rename", "replace", "utime", "chmod", "readlink",
    "pread", "pwrite", "dup", "dup2", "closerange", "get_blocking", "set_blocking",
    "get_inheritable", "set_inheritable", "lockf", "posix_fallocate",
)


def _efs_install(table):
    """Mount the file systems of this execution environment and serve them on `os`.

    `table` is what the HOST resolved the function's `FileSystemConfigs` to: where each file
    system is mounted, the identity requests through it are made as, whether this function's
    role may write to it, and the device it reports. The files themselves stay with the host,
    which holds the one tree every client of that file system shares.
    """
    _EFS_MOUNTS[:] = sorted(
        table.get("mounts") or [], key=lambda mount: len(mount["path"]), reverse=True
    )
    _EFS_ABOVE[:] = list(table.get("above") or [])
    _EFS_DESCRIPTORS.clear()
    _EFS_TIMES.clear()
    # The POSIX identity is the environment's whether or not it mounts anything.
    _efs_be_the_identity()
    if _EFS_MOUNTS:
        _efs_serve()


def _efs_be_the_identity():
    """Report the user and group the handler's process runs as.

    "The full POSIX identity […] on the access point […] used for all file system operations
    performed by NFS clients using the access point" (`PosixUser`) is the identity a function
    that mounts such an access point acts as; a function that mounts none runs as the user a
    Lambda execution environment runs as.
    """
    for mount in _EFS_MOUNTS:
        if mount.get("uid") is None:
            continue
        _EFS_IDENTITY["uid"] = int(mount["uid"])
        _EFS_IDENTITY["gid"] = int(mount["gid"])
        _EFS_IDENTITY["groups"] = [int(gid) for gid in (mount.get("secondaryGids") or [])]
        break
    os.getuid = _efs_getuid
    os.geteuid = _efs_getuid
    os.getgid = _efs_getgid
    os.getegid = _efs_getgid
    os.getgroups = _efs_getgroups


def _efs_getuid():
    return _EFS_IDENTITY["uid"]


def _efs_getgid():
    return _EFS_IDENTITY["gid"]


def _efs_getgroups():
    """The groups the process is in: the one it runs as, and the secondary groups the access
    point names beside it."""
    groups = [_EFS_IDENTITY["gid"]]
    for group in _EFS_IDENTITY["groups"]:
        if group not in groups:
            groups.append(group)
    return groups


def _efs_serve():
    """Put the dispatching calls on `os`, and the file opening on `io`, on `_io` and on the
    builtins.

    Each of them answers a path of one of these mounts HERE and hands every other path back to
    the call that was installed below it, so nothing this layer installs takes anything away
    from what the environment already had.
    """
    for name in _EFS_SERVED:
        _EFS_BELOW[name] = getattr(os, name)
    _EFS_BELOW["open_file"] = builtins.open
    os.stat = _efs_stat
    os.lstat = _efs_stat
    os.listdir = _efs_listdir
    os.scandir = _efs_scandir
    os.access = _efs_access
    os.open = _efs_open
    os.read = _efs_read
    os.write = _efs_write
    os.close = _efs_close
    os.lseek = _efs_lseek
    os.fstat = _efs_fstat
    os.fstatvfs = _efs_fstatvfs
    os.statvfs = _efs_statvfs
    os.ftruncate = _efs_ftruncate
    os.truncate = _efs_truncate
    os.fsync = _efs_fsync
    os.fdatasync = _efs_fsync
    os.mkdir = _efs_mkdir
    os.rmdir = _efs_rmdir
    os.unlink = _efs_unlink
    os.remove = _efs_unlink
    os.rename = _efs_rename
    os.replace = _efs_rename
    os.utime = _efs_utime
    os.chmod = _efs_chmod
    os.readlink = _efs_readlink
    os.pread = _efs_pread
    os.pwrite = _efs_pwrite
    os.dup = _efs_dup
    os.dup2 = _efs_dup2
    os.closerange = _efs_closerange
    os.get_blocking = _efs_get_blocking
    os.set_blocking = _efs_set_blocking
    os.get_inheritable = _efs_get_inheritable
    os.set_inheritable = _efs_set_inheritable
    os.lockf = _efs_lockf
    os.posix_fallocate = _efs_posix_fallocate
    builtins.open = _efs_open_file
    io.open = _efs_open_file
    __import__("_io").open = _efs_open_file


def _efs_ask(request):
    """What the HOST answers for one call on a path of these mounts."""
    return json.loads(_host.files(json.dumps(request)))


def _efs_mount_of(resolved):
    """The mount `resolved` lies in — the mount point itself, or anything under it — and `None`
    for a path on none of them. Only a SEPARATOR after the mount point puts a path inside it:
    `/mnt/datax` is not inside the mount at `/mnt/data`."""
    for mount in _EFS_MOUNTS:
        root = mount["path"]
        if resolved == root or resolved.startswith(root + "/"):
            return mount
    return None


def _efs_governs(resolved):
    """Whether this layer answers `resolved` at all: a path of one of the mounts, or a
    directory above one of them."""
    if resolved is None:
        return False
    return _efs_mount_of(resolved) is not None or resolved in _EFS_ABOVE


def _efs_refused(answered, resolved):
    """Raise what the host's refusal of a call on `resolved` means, as the operating system
    raises it. The errno is this layer's: the host names the condition, not the number."""
    refused = answered.get("refused")
    if refused == "read_only":
        raise OSError(errno.EROFS, _EFS_READ_ONLY, resolved)
    if refused == "absent":
        raise FileNotFoundError(errno.ENOENT, "No such file or directory", resolved)
    if refused == "is_a_directory":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    if refused == "not_a_directory":
        raise NotADirectoryError(errno.ENOTDIR, "Not a directory", resolved)
    if refused == "exists":
        raise FileExistsError(errno.EEXIST, "File exists", resolved)
    if refused == "not_empty":
        raise OSError(errno.ENOTEMPTY, "Directory not empty", resolved)
    if refused == "busy":
        raise OSError(errno.EBUSY, "Device or resource busy", resolved)
    if refused == "no_space":
        raise OSError(errno.ENOSPC, "No space left on device", resolved)
    if refused == "cross_device":
        raise OSError(errno.EXDEV, "Invalid cross-device link", resolved)
    if refused == "invalid":
        # A directory moved inside itself, and anything else the call cannot mean.
        raise OSError(errno.EINVAL, "Invalid argument", resolved)
    # The file system behind the mount is gone, which is what a stale handle says of a mount
    # whose server no longer holds it.
    raise OSError(errno.ESTALE, "Stale file handle", resolved)


def _efs_answered(request, resolved):
    """One call on the host, with its refusals raised."""
    answered = _efs_ask(request)
    if answered.get("refused"):
        _efs_refused(answered, resolved)
    return answered


def _efs_entry(resolved):
    """What the mounts hold at `resolved` — its kind, its size, when it was last written, the
    device it is on and who it belongs to — or `None` for a path they hold nothing at."""
    return _efs_answered({"op": "stat", "path": resolved}, resolved).get("entry")


def _efs_absent(resolved):
    return FileNotFoundError(errno.ENOENT, "No such file or directory", resolved)


def _efs_inode(resolved):
    if resolved not in _EFS_INODES:
        _EFS_INODES[resolved] = len(_EFS_INODES) + 1
    return _EFS_INODES[resolved]


def _efs_mode(resolved, entry):
    """The mode the entry at `resolved` reports: the permissions it carries, with the type bits
    of what it IS."""
    permissions = int(entry.get("mode") or 0) & 0o7777
    if entry["kind"] == "directory":
        return 0o040000 | permissions
    return 0o100000 | permissions


def _efs_stat_result(resolved, entry):
    """What a `stat` answers for the entry at `resolved`."""
    directory = entry["kind"] == "directory"
    size = 0 if directory else int(entry["size"])
    at = _EFS_TIMES.get(resolved, float(entry["modified"]))
    nanoseconds = int(at * 1000000000.0)
    uid = entry.get("uid")
    gid = entry.get("gid")
    return os.stat_result(
        (
            _efs_mode(resolved, entry),
            _efs_inode(resolved),
            # The DEVICE the filesystem reports: a mount of its own, which is what tells a
            # program the mount point is one rather than part of the directory above it.
            int(entry.get("device") or 0),
            1,
            _EFS_SANDBOX_UID if uid is None else int(uid),
            _EFS_SANDBOX_GID if gid is None else int(gid),
            size,
            at,
            at,
            at,
        ),
        {
            "st_atime_ns": nanoseconds,
            "st_mtime_ns": nanoseconds,
            "st_ctime_ns": nanoseconds,
            "st_blksize": 4096,
            "st_blocks": (size + 511) // 512,
        },
    )


def _efs_stat(path, *, dir_fd=None, follow_symlinks=True):
    """What a `stat` answers for a path of this environment: these mounts first, then whatever
    the layer below holds.

    Nothing under these mounts is a symbolic link, so following links or not is the same
    reading, which is why this answers `lstat` as well.
    """
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["stat"](path, dir_fd=dir_fd, follow_symlinks=follow_symlinks)
    entry = _efs_entry(resolved)
    if entry is None:
        raise _efs_absent(resolved)
    return _efs_stat_result(resolved, entry)


def _efs_listdir(path="."):
    """The names the directory at `path` holds."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["listdir"](path)
    return list(_efs_answered({"op": "list", "path": resolved}, resolved).get("names") or [])


class _EfsEntry:
    """One entry of a directory of these mounts, as `scandir` answers them."""

    def __init__(self, directory, name):
        self.name = name
        self.path = directory + "/" + name if directory != "/" else "/" + name

    def __repr__(self):
        return "<DirEntry %r>" % (self.name,)

    def __fspath__(self):
        return self.path

    def inode(self):
        return _efs_inode(self.path)

    def _kind(self):
        entry = _efs_entry(self.path)
        return None if entry is None else entry["kind"]

    def is_dir(self, *, follow_symlinks=True):
        return self._kind() == "directory"

    def is_file(self, *, follow_symlinks=True):
        return self._kind() == "file"

    def is_symlink(self):
        return False

    def stat(self, *, follow_symlinks=True):
        entry = _efs_entry(self.path)
        if entry is None:
            raise _efs_absent(self.path)
        return _efs_stat_result(self.path, entry)


class _EfsScan:
    """The entries of one directory of these mounts, as the iterator `scandir` answers."""

    def __init__(self, directory, names):
        self._directory = directory
        self._names = list(names)
        self._at = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self._at >= len(self._names):
            raise StopIteration
        name = self._names[self._at]
        self._at += 1
        return _EfsEntry(self._directory, name)

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    def close(self):
        self._names = []
        self._at = 0


def _efs_scandir(path="."):
    """The entries of the directory at `path`, as objects naming what each one is."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["scandir"](path)
    return _EfsScan(resolved, _efs_listdir(resolved))


def _efs_access(path, mode, *, dir_fd=None, effective_ids=False, follow_symlinks=True):
    """Whether the path is reachable in the way `mode` asks: a file of a mount can be read,
    and written only where this function's role may write to that file system."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["access"](
            path,
            mode,
            dir_fd=dir_fd,
            effective_ids=effective_ids,
            follow_symlinks=follow_symlinks,
        )
    try:
        entry = _efs_entry(resolved)
    except OSError:
        return False
    if entry is None:
        return False
    mount = _efs_mount_of(resolved)
    if mode & os.W_OK and not (mount and mount.get("writable")):
        return False
    return True


def _efs_statvfs(path):
    """What a `statvfs` answers for the file system a path of these mounts is on."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["statvfs"](path)
    held = _efs_answered({"op": "statvfs", "path": resolved}, resolved)
    block = int(held["blockSize"])
    return os.statvfs_result(
        (
            block,
            block,
            int(held["blocks"]),
            int(held["free"]),
            int(held["free"]),
            int(held["files"]),
            int(held["freeFiles"]),
            int(held["freeFiles"]),
            os.ST_RDONLY if held.get("readOnly") else 0,
            _EFS_MAX_NAME,
        ),
        {"f_fsid": 0},
    )


def _efs_fstatvfs(descriptor):
    """The same, for the file system a descriptor's own file is on."""
    if descriptor in _EFS_DESCRIPTORS:
        return _efs_statvfs(_EFS_DESCRIPTORS[descriptor][0])
    return _EFS_BELOW["fstatvfs"](descriptor)


def _efs_slice(resolved, at=0, size=None):
    """The bytes the file at `resolved` holds from `at` on, at most `size` of them, and how
    many bytes the whole file holds.

    One call per read, carrying only the part asked for: the content travels base64, which is
    how a document carries bytes that are no text at all.
    """
    request = {"op": "read", "path": resolved, "from": max(0, int(at))}
    if size is not None:
        request["size"] = max(0, int(size))
    held = _efs_answered(request, resolved)
    return base64.b64decode(held["bytes"]), int(held["size"])


def _efs_bytes(resolved):
    """The whole content of the file at `resolved`, which is the file itself."""
    return _efs_slice(resolved)[0]


def _efs_size(resolved):
    """How many bytes the file at `resolved` holds."""
    entry = _efs_entry(resolved)
    if entry is None:
        raise _efs_absent(resolved)
    return int(entry["size"])


def _efs_appended(resolved, data):
    """Put `data` at the END of the file at `resolved`, which is what a write there is: only
    the bytes written travel, so a program writing a file through costs one call per write
    rather than one copy of the whole file."""
    _efs_answered(
        {
            "op": "write",
            "path": resolved,
            "bytes": base64.b64encode(bytes(data)).decode("ascii"),
            "append": True,
        },
        resolved,
    )


def _efs_put(resolved, data):
    """Hold `data` as the whole content of the file at `resolved`."""
    _efs_answered(
        {
            "op": "write",
            "path": resolved,
            "bytes": base64.b64encode(bytes(data)).decode("ascii"),
            "append": False,
        },
        resolved,
    )


def _efs_truncate_to(resolved, size):
    """Make the file at `resolved` exactly `size` bytes long."""
    _efs_answered({"op": "truncate", "path": resolved, "size": max(0, int(size))}, resolved)
    return max(0, int(size))


def _efs_written_now(resolved):
    """A write of the entry at `resolved` happened, so a time a program SET on it no longer
    stands: what it reports is the instant of the write the file system recorded."""
    _EFS_TIMES.pop(resolved, None)


def _efs_create(resolved):
    """Create an empty file at `resolved`."""
    _efs_answered(
        {"op": "write", "path": resolved, "bytes": "", "append": False}, resolved
    )
    _efs_written_now(resolved)


def _efs_descriptor_for(resolved, appending, readable, writable):
    """A descriptor open on the file at `resolved`: the number a caller holds, how far into the
    file it is, and what it may do."""
    descriptor = _tmp_next_descriptor(resolved)
    position = _efs_size(resolved) if appending else 0
    _EFS_DESCRIPTORS[descriptor] = [
        resolved,
        position,
        bool(writable),
        bool(appending),
        bool(readable),
    ]
    return descriptor


def _efs_opened(resolved, creating, exclusive, truncating, appending, readable, writable):
    """Open the file at `resolved`, as both forms of an open do: the mode or the flags say
    whether it is created, truncated, appended to, read and written."""
    mount = _efs_mount_of(resolved)
    if (creating or truncating or writable) and not (mount and mount.get("writable")):
        raise OSError(errno.EROFS, _EFS_READ_ONLY, resolved)
    entry = _efs_entry(resolved)
    if entry is not None and entry["kind"] == "directory":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    if entry is not None and exclusive:
        raise FileExistsError(errno.EEXIST, "File exists", resolved)
    if entry is None:
        if not creating:
            raise _efs_absent(resolved)
        _efs_create(resolved)
    elif truncating:
        _efs_truncate_to(resolved, 0)
    return _efs_descriptor_for(
        resolved, appending=appending, readable=readable, writable=writable
    )


def _efs_open(path, flags, mode=0o777, *, dir_fd=None):
    """Open a file of these mounts as a DESCRIPTOR, as the operating-system call does."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["open"](path, flags, mode, dir_fd=dir_fd)
    return _efs_opened(
        resolved,
        creating=bool(flags & os.O_CREAT),
        exclusive=bool(flags & os.O_CREAT and flags & os.O_EXCL),
        truncating=bool(flags & os.O_TRUNC),
        appending=bool(flags & os.O_APPEND),
        readable=not flags & os.O_WRONLY,
        writable=bool(flags & (os.O_WRONLY | os.O_RDWR | os.O_APPEND | os.O_CREAT | os.O_TRUNC)),
    )


def _efs_read(descriptor, size):
    """The next `size` bytes of a descriptor open on a file of these mounts."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["read"](descriptor, size)
    held = _EFS_DESCRIPTORS[descriptor]
    if not held[4]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    read, _ = _efs_slice(held[0], held[1], max(0, size))
    held[1] = held[1] + len(read)
    return read


def _efs_write(descriptor, data):
    """A write on such a descriptor, which reaches the file system where it is made."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["write"](descriptor, data)
    held = _EFS_DESCRIPTORS[descriptor]
    if not held[2]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    chunk = bytes(data)
    size = _efs_size(held[0])
    at = size if held[3] else held[1]
    if at == size:
        # At the END of the file, which is where a program writing one through writes: only
        # the bytes written travel.
        _efs_appended(held[0], chunk)
    else:
        # Inside it: the file is the one the write lands in, so what it holds is read, the
        # chunk put at the position, and the file written back.
        bytes_of = bytearray(_efs_bytes(held[0]))
        if at > len(bytes_of):
            bytes_of.extend(bytes(at - len(bytes_of)))
        bytes_of[at:at + len(chunk)] = chunk
        _efs_put(held[0], bytes_of)
    _efs_written_now(held[0])
    held[1] = at + len(chunk)
    return len(chunk)


def _efs_close(descriptor):
    """Close such a descriptor."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["close"](descriptor)
    del _EFS_DESCRIPTORS[descriptor]
    return None


def _efs_lseek(descriptor, position, whence=0):
    """Move where such a descriptor is reading from."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["lseek"](descriptor, position, whence)
    held = _EFS_DESCRIPTORS[descriptor]
    if whence == 1:
        position = held[1] + position
    elif whence == 2:
        position = _efs_size(held[0]) + position
    if position < 0:
        raise OSError(errno.EINVAL, "Invalid argument")
    held[1] = position
    return position


def _efs_fstat(descriptor):
    """What a `stat` answers for the file such a descriptor is open on."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["fstat"](descriptor)
    resolved = _EFS_DESCRIPTORS[descriptor][0]
    entry = _efs_entry(resolved)
    if entry is None:
        raise _efs_absent(resolved)
    return _efs_stat_result(resolved, entry)


def _efs_ftruncate(descriptor, size):
    """Make the file such a descriptor is open on exactly `size` bytes long."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["ftruncate"](descriptor, size)
    held = _EFS_DESCRIPTORS[descriptor]
    if not held[2]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    _efs_truncate_to(held[0], size)
    _efs_written_now(held[0])
    return None


def _efs_truncate(path, size):
    """The same, by path."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["truncate"](path, size)
    if _efs_entry(resolved) is None:
        raise _efs_absent(resolved)
    _efs_truncate_to(resolved, size)
    _efs_written_now(resolved)
    return None


def _efs_fsync(descriptor):
    """Flush such a descriptor, which is written where it was written."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["fsync"](descriptor)
    return None


def _efs_pread(descriptor, size, offset):
    """The bytes at `offset` of the file such a descriptor is open on, leaving where it is
    reading from where it was."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["pread"](descriptor, size, offset)
    held = _EFS_DESCRIPTORS[descriptor]
    if not held[4]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    read, _ = _efs_slice(held[0], max(0, offset), max(0, size))
    return read


def _efs_pwrite(descriptor, data, offset):
    """The same for a write: `data` lands at `offset` and the position is left where it was."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["pwrite"](descriptor, data, offset)
    held = _EFS_DESCRIPTORS[descriptor]
    if not held[2]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    at = held[1]
    appending = held[3]
    held[1] = max(0, offset)
    held[3] = False
    try:
        return _efs_write(descriptor, data)
    finally:
        held[1] = at
        held[3] = appending


def _efs_dup(descriptor):
    """A second descriptor on the same open file, as `dup` hands one out."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["dup"](descriptor)
    second = _tmp_next_descriptor(_EFS_DESCRIPTORS[descriptor][0])
    _EFS_DESCRIPTORS[second] = list(_EFS_DESCRIPTORS[descriptor])
    return second


def _efs_dup2(descriptor, second, inheritable=True):
    """The same, onto the number `second`, which is closed first if it was open."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["dup2"](descriptor, second, inheritable)
    if second < 0 or second >= _TMP_MAX_DESCRIPTORS:
        # A number outside the descriptors this environment has is not one it can hold.
        raise OSError(errno.EBADF, "Bad file descriptor")
    _EFS_DESCRIPTORS[second] = list(_EFS_DESCRIPTORS[descriptor])
    return second


def _efs_closerange(first, last):
    """Close every descriptor of this environment in the range, these mounts' included: one
    that is not open is passed over."""
    for descriptor in list(_EFS_DESCRIPTORS):
        if first <= descriptor <= last:
            del _EFS_DESCRIPTORS[descriptor]
    return _EFS_BELOW["closerange"](first, last)


def _efs_get_blocking(descriptor):
    """Whether such a descriptor blocks, which a file of a mount always does: every read and
    write of it is answered from this process and none of them waits."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["get_blocking"](descriptor)
    return True


def _efs_set_blocking(descriptor, blocking):
    """Ask such a descriptor not to block, which changes nothing."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["set_blocking"](descriptor, blocking)
    return None


def _efs_get_inheritable(descriptor):
    """Whether such a descriptor would be inherited by a child process: this environment
    starts none, and one it hands out is not inheritable."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["get_inheritable"](descriptor)
    return False


def _efs_set_inheritable(descriptor, inheritable):
    """Ask for one to be inheritable, which reaches nothing."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["set_inheritable"](descriptor, inheritable)
    return None


def _efs_lockf(descriptor, command, length, start=0, whence=0):
    """Take or release an advisory lock on such a descriptor's file.

    An execution environment is one process with one invocation in it, so no other holder of a
    lock on a file of these mounts exists in it: a lock asked for is held, and one released
    was.
    """
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["lockf"](descriptor, command, length, start, whence)
    return None


def _efs_posix_fallocate(descriptor, offset, length):
    """Give the file such a descriptor is open on the space `offset` and `length` name, which
    is what the call reserves: the file is made that long."""
    if descriptor not in _EFS_DESCRIPTORS:
        return _EFS_BELOW["posix_fallocate"](descriptor, offset, length)
    held = _EFS_DESCRIPTORS[descriptor]
    if not held[2]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    wanted = max(0, offset) + max(0, length)
    if wanted > _efs_size(held[0]):
        _efs_truncate_to(held[0], wanted)
    return None


def _efs_mkdir(path, mode=0o777, *, dir_fd=None):
    """Make a directory under one of these mounts."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["mkdir"](path, mode, dir_fd=dir_fd)
    _efs_answered(
        {"op": "mkdir", "path": resolved, "mode": (mode & 0o7777) & ~_EFS_MASK}, resolved
    )
    return None


def _efs_rmdir(path, *, dir_fd=None):
    """Remove an empty directory under one of these mounts."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["rmdir"](path, dir_fd=dir_fd)
    _efs_answered({"op": "rmdir", "path": resolved}, resolved)
    _EFS_TIMES.pop(resolved, None)
    return None


def _efs_unlink(path, *, dir_fd=None):
    """Remove a file under one of these mounts."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["unlink"](path, dir_fd=dir_fd)
    _efs_answered({"op": "remove", "path": resolved}, resolved)
    _EFS_TIMES.pop(resolved, None)
    return None


def _efs_rename(source, target, *, src_dir_fd=None, dst_dir_fd=None):
    """Move an entry of one of these mounts to another path of the same file system."""
    from_path = _mount_resolved(source)
    to_path = _mount_resolved(target)
    if not _efs_governs(from_path) and not _efs_governs(to_path):
        return _EFS_BELOW["rename"](
            source, target, src_dir_fd=src_dir_fd, dst_dir_fd=dst_dir_fd
        )
    if from_path is None or to_path is None:
        raise OSError(errno.EINVAL, "Invalid argument")
    if not _efs_governs(from_path) or not _efs_governs(to_path):
        # One of the two paths is on another filesystem of this environment: a name does not
        # move from one to the other.
        raise OSError(errno.EXDEV, "Invalid cross-device link", to_path)
    _efs_answered(
        {"op": "rename", "path": from_path, "target": to_path}, from_path
    )
    if from_path in _EFS_TIMES:
        _EFS_TIMES[to_path] = _EFS_TIMES.pop(from_path)
    return None


def _efs_utime(path, times=None, *, ns=None, dir_fd=None, follow_symlinks=True):
    """Set when an entry of one of these mounts was last written."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["utime"](
            path, times, ns=ns, dir_fd=dir_fd, follow_symlinks=follow_symlinks
        )
    mount = _efs_mount_of(resolved)
    if not (mount and mount.get("writable")):
        raise OSError(errno.EROFS, _EFS_READ_ONLY, resolved)
    if _efs_entry(resolved) is None:
        raise _efs_absent(resolved)
    if ns is not None:
        _EFS_TIMES[resolved] = ns[1] / 1000000000.0
    elif times is not None:
        _EFS_TIMES[resolved] = times[1]
    else:
        _EFS_TIMES.pop(resolved, None)
    return None


def _efs_chmod(path, mode, *, dir_fd=None, follow_symlinks=True):
    """Set the permissions an entry of one of these mounts carries, which the file system holds
    with the entry."""
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["chmod"](
            path, mode, dir_fd=dir_fd, follow_symlinks=follow_symlinks
        )
    _efs_answered(
        {"op": "chmod", "path": resolved, "mode": mode & 0o7777}, resolved
    )
    return None


def _efs_readlink(path, *, dir_fd=None):
    """What the symbolic link at `path` points to.

    Nothing under these mounts is a symbolic link — the file system holds the files and
    directories its clients wrote — so a path it holds is not a link, which is what `EINVAL`
    says of one.
    """
    resolved = _mount_resolved(path)
    if not _efs_governs(resolved):
        return _EFS_BELOW["readlink"](path, dir_fd=dir_fd)
    if _efs_entry(resolved) is None:
        raise _efs_absent(resolved)
    raise OSError(errno.EINVAL, "Invalid argument", resolved)


class _EfsStream:
    """One open file of a mounted file system, as `open` answers with one.

    It is a stream over a DESCRIPTOR of this layer, as a file object of a real runtime is: how
    far into the file it is lives in the descriptor, so `os.write(stream.fileno(), ...)` and the
    stream's own `write` are two ways into one open file — and a write reaches the file system
    where the handler made it, which is what makes it a write another client of that file
    system reads.
    """

    def __init__(self, descriptor, mode, text, encoding, errors):
        self.name = _EFS_DESCRIPTORS[descriptor][0]
        self.mode = mode
        self.encoding = encoding if text else None
        self.errors = errors if text else None
        self.newlines = None
        self.closed = False
        self._descriptor = descriptor
        self._text = text

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    def __iter__(self):
        return self

    def __next__(self):
        line = self.readline()
        if not line:
            raise StopIteration
        return line

    def _open(self):
        """The descriptor this stream is on, which is where how far into the file it is lives."""
        if self.closed:
            raise ValueError("I/O operation on closed file")
        if self._descriptor not in _EFS_DESCRIPTORS:
            raise OSError(errno.EBADF, "Bad file descriptor")
        return _EFS_DESCRIPTORS[self._descriptor]

    def _decoded(self, chunk):
        if not self._text:
            return chunk
        return chunk.decode(self.encoding or "utf-8", self.errors or "strict")

    def read(self, size=-1):
        held = self._open()
        if not held[4]:
            raise io.UnsupportedOperation("not readable")
        asked = None if size is None or size < 0 else size
        chunk, _ = _efs_slice(held[0], held[1], asked)
        held[1] = held[1] + len(chunk)
        return self._decoded(chunk)

    def readline(self, size=-1):
        """The next line, read a CHUNK at a time: a line is however far the next break is, so
        the stream asks for the file a chunk at a time rather than for all of it."""
        held = self._open()
        if not held[4]:
            raise io.UnsupportedOperation("not readable")
        limit = None if size is None or size < 0 else size
        line = bytearray()
        at = held[1]
        while True:
            asked = _EFS_LINE_CHUNK if limit is None else min(_EFS_LINE_CHUNK, limit - len(line))
            if asked <= 0:
                break
            chunk, _ = _efs_slice(held[0], at, asked)
            if not chunk:
                break
            found = chunk.find(b"\n")
            if found < 0:
                line.extend(chunk)
                at += len(chunk)
                continue
            line.extend(chunk[:found + 1])
            at += found + 1
            break
        held[1] = at
        return self._decoded(bytes(line))

    def readlines(self, hint=-1):
        lines = []
        while True:
            line = self.readline()
            if not line:
                return lines
            lines.append(line)

    def write(self, data):
        held = self._open()
        if not held[2]:
            raise io.UnsupportedOperation("not writable")
        if self._text:
            chunk = data.encode(self.encoding or "utf-8", self.errors or "strict")
        else:
            chunk = bytes(data)
        _efs_write(self._descriptor, chunk)
        return len(data)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def seek(self, position, whence=0):
        self._open()
        return _efs_lseek(self._descriptor, position, whence)

    def tell(self):
        return self._open()[1]

    def truncate(self, size=None):
        held = self._open()
        if not held[2]:
            raise io.UnsupportedOperation("not writable")
        return _efs_truncate_to(held[0], held[1] if size is None else size)

    def flush(self):
        self._open()
        return None

    def close(self):
        if not self.closed:
            self.closed = True
            _EFS_DESCRIPTORS.pop(self._descriptor, None)
        return None

    def readable(self):
        return bool(self._open()[4])

    def writable(self):
        return bool(self._open()[2])

    def seekable(self):
        return True

    def isatty(self):
        return False

    def fileno(self):
        if self.closed:
            raise ValueError("I/O operation on closed file")
        return self._descriptor


def _efs_open_file(
    file,
    mode="r",
    buffering=-1,
    encoding=None,
    errors=None,
    newline=None,
    closefd=True,
    opener=None,
):
    """Open a file of one of these mounts, as `open` does."""
    if isinstance(file, int) and not isinstance(file, bool):
        if file in _EFS_DESCRIPTORS:
            return _EfsStream(file, mode, "b" not in mode, _tmp_encoding(encoding), errors)
        return _EFS_BELOW["open_file"](
            file, mode, buffering, encoding, errors, newline, closefd, opener
        )
    resolved = _mount_resolved(file)
    if not _efs_governs(resolved):
        return _EFS_BELOW["open_file"](
            file, mode, buffering, encoding, errors, newline, closefd, opener
        )
    updating = "+" in mode
    descriptor = _efs_opened(
        resolved,
        creating=any(character in mode for character in "wax"),
        exclusive="x" in mode,
        truncating="w" in mode,
        appending="a" in mode,
        readable="r" in mode or updating,
        writable=any(character in mode for character in "wax") or updating,
    )
    return _EfsStream(descriptor, mode, "b" not in mode, _tmp_encoding(encoding), errors)
"##,
            $($runtime)*
        )
    };
}

pub(super) use file_system_source;

#[cfg(test)]
mod tests {
    use super::{GuestFileSystems, BLOCK_SIZE};
    use crate::state::FileSystemMount;
    use ::serde_json::{json, Value};

    /// A mount of `files` at `path`, rooted at `root`, writable or not, enforcing the identity
    /// `posix` where the case names one.
    fn mounted(
        path: &str,
        root: &str,
        writable: bool,
        posix: Option<(i64, i64)>,
        files: &super::efs::MountedFileSystem,
    ) -> FileSystemMount {
        FileSystemMount {
            local_mount_path: path.to_string(),
            file_system_id: "fs-0123456789abcdef0".to_string(),
            access_point_id: "fsap-0123456789abcdef0".to_string(),
            root_directory: root.to_string(),
            uid: posix.map(|(uid, _)| uid),
            gid: posix.map(|(_, gid)| gid),
            secondary_gids: Vec::new(),
            // The access point's own root directory, created with the identity and the
            // permissions its `CreationInfo` states.
            root_creation: Some((7001, 7002, 0o750)),
            writable,
            device: 42,
            files: files.clone(),
        }
    }

    /// [`mounted`] for the access point every case but the identity ones uses: one enforcing
    /// the POSIX user `1001:1001`.
    fn mount(
        path: &str,
        root: &str,
        writable: bool,
        files: &super::efs::MountedFileSystem,
    ) -> FileSystemMount {
        mounted(path, root, writable, Some((1001, 1001)), files)
    }

    /// One call on the seam, as the guest makes it.
    fn asked(systems: &mut GuestFileSystems, request: &Value) -> Value {
        ::serde_json::from_str(&systems.answered(&request.to_string()))
            .expect("the seam answers a document")
    }

    /// A write of `bytes` at `path`, as the guest makes one: the content travels base64.
    fn write(path: &str, bytes: &[u8]) -> Value {
        json!({ "op": "write", "path": path, "bytes": super::encoded(bytes) })
    }

    /// What a read answered, as the bytes the file holds — and `None` for a refusal.
    fn held(answered: &Value) -> Option<Vec<u8>> {
        answered["bytes"].as_str().map(super::decoded)
    }

    /// RULE: a path under a mount point is read and written in the FILE SYSTEM's own tree, so
    /// what one mount writes another mount of the same file system reads — bytes that are no
    /// text at all included, because a file of a filesystem is its bytes — and a path on no
    /// mount of this function is not answered here at all.
    #[test]
    fn a_write_under_one_mount_is_read_under_another_of_the_same_file_system() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        let mut first = GuestFileSystems::attach(&[mount("/mnt/data", "/shared", true, &files)]);
        let mut second = GuestFileSystems::attach(&[mount("/mnt/other", "/shared", true, &files)]);
        // (case, the name, the bytes written)
        let rows: Vec<(&str, &str, Vec<u8>)> = vec![
            ("text", "note.txt", b"held".to_vec()),
            ("bytes that are no text at all", "blob.bin", vec![0, 1, 2, 0x7f, 0xff, 0xfe]),
            ("nothing at all", "empty.bin", Vec::new()),
        ];
        for (case, name, bytes) in rows {
            let wrote = asked(&mut first, &write(&format!("/mnt/data/{name}"), &bytes));
            assert_eq!(
                wrote,
                json!({"ok": true}),
                "case {case:?}: the mount is writable, so the write happens, got {wrote}"
            );
            let read = asked(
                &mut second,
                &json!({"op": "read", "path": format!("/mnt/other/{name}")}),
            );
            assert_eq!(
                held(&read),
                Some(bytes.clone()),
                "case {case:?}: the second mount reads the same file system, got {read}"
            );
        }
        let outside = asked(&mut first, &json!({"op": "read", "path": "/var/task/index.py"}));
        assert_eq!(
            outside,
            json!({"refused": "unreachable"}),
            "a path on no mount of this function is not answered here, got {outside}"
        );
    }

    /// One row of the table below: the case, the identity the access point enforces, and the
    /// owner its entries report.
    type OwnerRow = (&'static str, Option<(i64, i64)>, (i64, i64));

    /// RULE: an entry BELONGS TO the identity that created it — the `PosixUser` of the access
    /// point it was written through, or the user a function runs as where that access point
    /// enforces none — and that owner travels with the entry, so every other client of the file
    /// system reports the same one.
    #[test]
    fn an_entry_belongs_to_the_identity_that_created_it() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        // (case, the identity the access point enforces, the owner its entries report)
        let rows: Vec<OwnerRow> = vec![
            ("an access point enforcing an identity", Some((1001, 1002)), (1001, 1002)),
            ("another identity entirely", Some((4242, 4243)), (4242, 4243)),
            ("an access point enforcing none", None, (super::SANDBOX_UID, super::SANDBOX_GID)),
        ];
        for (case, posix, (uid, gid)) in rows {
            let name = format!("/mnt/data/{uid}.txt");
            let mut writer = GuestFileSystems::attach(&[mounted(
                "/mnt/data",
                "/shared",
                true,
                posix,
                &files,
            )]);
            asked(&mut writer, &write(&name, b"held"));
            asked(&mut writer, &json!({"op": "mkdir", "path": format!("/mnt/data/{uid}")}));
            // Read back through an access point of ANOTHER identity: the owner is the
            // writer's, not the reader's.
            let mut reader = GuestFileSystems::attach(&[mounted(
                "/mnt/data",
                "/shared",
                true,
                Some((7, 7)),
                &files,
            )]);
            for path in [name.clone(), format!("/mnt/data/{uid}")] {
                let stat = asked(&mut reader, &json!({"op": "stat", "path": path}));
                assert_eq!(
                    (stat["entry"]["uid"].clone(), stat["entry"]["gid"].clone()),
                    (json!(uid), json!(gid)),
                    "case {case:?}: expected {path} to belong to {uid}:{gid}, got {stat}"
                );
            }
        }
    }

    /// RULE: the access point's root directory is the ROOT of what the function can reach, so
    /// another access point of the same file system sees its own subtree only and a path that
    /// climbs out of the mount point is no longer in the mount — and the mount point reports
    /// the mount's own device, which is what says it is a mount point.
    #[test]
    fn the_access_point_root_confines_the_function() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        let mut systems = GuestFileSystems::attach(&[mount("/mnt/data", "/one", true, &files)]);
        let mut neighbour =
            GuestFileSystems::attach(&[mount("/mnt/data", "/two", true, &files)]);
        let wrote = asked(&mut systems, &write("/mnt/data/held.txt", b"out"));
        assert_eq!(
            wrote,
            json!({"ok": true}),
            "the write happens under the access point's own root, got {wrote}"
        );
        let other = asked(
            &mut neighbour,
            &json!({"op": "read", "path": "/mnt/data/held.txt"}),
        );
        assert_eq!(
            other,
            json!({"refused": "absent"}),
            "another access point of the same file system sees its own subtree only, got {other}"
        );
        // A path climbing above the mount point resolves to the directory above it, which is
        // the environment's own read-only root filesystem rather than anything of the mount.
        let escaped = asked(&mut systems, &write("/mnt/data/../escaped.txt", b"out"));
        assert_eq!(
            escaped,
            json!({"refused": "read_only"}),
            "nothing of the file system lies outside the mount point, got {escaped}"
        );
        let stat = asked(&mut systems, &json!({"op": "stat", "path": "/mnt/data"}));
        assert_eq!(
            stat["entry"]["device"],
            json!(42),
            "the mount point reports the file system's own device, got {stat}"
        );
        let above = asked(&mut systems, &json!({"op": "stat", "path": "/mnt"}));
        assert_eq!(
            above["entry"]["device"],
            json!(0),
            "the directory above it is the environment's own storage, got {above}"
        );
    }

    /// RULE: the access point's own ROOT DIRECTORY is created with the identity and the
    /// permissions its `CreationInfo` states, and those travel with the directory — so a
    /// client reading the file system through another access point reports them.
    #[test]
    fn the_access_point_root_is_created_with_its_creation_info() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        // Attaching the access point creates its root directory; the whole file system is
        // then read through a mount of its root, which is a client of another identity.
        let _confined = GuestFileSystems::attach(&[mount("/mnt/data", "/held", true, &files)]);
        let mut whole = GuestFileSystems::attach(&[mounted(
            "/mnt/fs",
            "/",
            true,
            Some((0, 0)),
            &files,
        )]);
        let stat = asked(&mut whole, &json!({"op": "stat", "path": "/mnt/fs/held"}));
        assert_eq!(
            (
                stat["entry"]["kind"].clone(),
                stat["entry"]["uid"].clone(),
                stat["entry"]["gid"].clone(),
                stat["entry"]["mode"].clone(),
            ),
            (json!("directory"), json!(7001), json!(7002), json!(0o750)),
            "the root directory carries what the access point's CreationInfo states, got {stat}"
        );
    }

    /// RULE: a function whose role may not write has the file system mounted READ-ONLY — every
    /// write under the mount point is refused, and the reads still answer.
    #[test]
    fn a_mount_without_the_write_grant_is_read_only() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        let mut writer = GuestFileSystems::attach(&[mount("/mnt/data", "/shared", true, &files)]);
        asked(&mut writer, &write("/mnt/data/note.txt", b"held"));
        let mut reader = GuestFileSystems::attach(&[mount("/mnt/data", "/shared", false, &files)]);
        // (case, the call, what it is refused with)
        let rows: Vec<(&str, Value, &str)> = vec![
            ("a write", write("/mnt/data/second.txt", b"no"), "read_only"),
            ("a directory", json!({"op": "mkdir", "path": "/mnt/data/deep"}), "read_only"),
            ("a removal", json!({"op": "remove", "path": "/mnt/data/note.txt"}), "read_only"),
            (
                "a rename",
                json!({"op": "rename", "path": "/mnt/data/note.txt", "target": "/mnt/data/moved.txt"}),
                "read_only",
            ),
            (
                "a write above the mount points",
                write("/mnt/stray.txt", b"no"),
                "read_only",
            ),
        ];
        for (case, request, expected) in rows {
            let answered = asked(&mut reader, &request);
            assert_eq!(
                answered,
                json!({ "refused": expected }),
                "case {case:?}: expected the refusal {expected:?}, got {answered}"
            );
        }
        let read = asked(&mut reader, &json!({"op": "read", "path": "/mnt/data/note.txt"}));
        assert_eq!(
            held(&read),
            Some(b"held".to_vec()),
            "a read-only mount still reads, got {read}"
        );
    }

    /// RULE: a path the FILE SYSTEM has no room for, and a call that cannot mean anything, are
    /// refused before anything is held for them: a truncate is decided from the size it asks
    /// for, and a directory does not move inside itself.
    #[test]
    fn a_call_the_file_system_has_no_room_for_is_refused_before_anything_is_held() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        let mut systems = GuestFileSystems::attach(&[mount("/mnt/data", "/shared", true, &files)]);
        asked(&mut systems, &write("/mnt/data/note.txt", b"held"));
        asked(&mut systems, &json!({"op": "mkdir", "path": "/mnt/data/deep"}));
        asked(&mut systems, &write("/mnt/data/deep/held.txt", b"deeper"));
        let room = super::efs::FILE_SYSTEM_LIMITS.bytes;
        // (case, the call, what it is refused with) — the sizes span the published ceiling of
        // one file system, one byte past it, and the largest a request can even name.
        let rows: Vec<(&str, Value, &str)> = vec![
            (
                "a size past what one file system holds",
                json!({"op": "truncate", "path": "/mnt/data/note.txt", "size": room + 1}),
                "no_space",
            ),
            (
                "a size no host could name",
                json!({"op": "truncate", "path": "/mnt/data/note.txt", "size": u64::MAX}),
                "no_space",
            ),
            (
                "a directory moved into itself",
                json!({"op": "rename", "path": "/mnt/data/deep", "target": "/mnt/data/deep/held"}),
                "invalid",
            ),
            (
                "a directory moved onto itself",
                json!({"op": "rename", "path": "/mnt/data/deep", "target": "/mnt/data/deep"}),
                "invalid",
            ),
        ];
        for (case, request, expected) in rows {
            let answered = asked(&mut systems, &request);
            assert_eq!(
                answered,
                json!({ "refused": expected }),
                "case {case:?}: expected the refusal {expected:?}, got {answered}"
            );
        }
        // And the sizes it DOES have room for are performed: the file grows reading as zero
        // bytes where it grew, and shrinks to what was asked.
        for (case, size, expected) in [("grown", 8_usize, b"held\0\0\0\0".to_vec()), ("shrunk", 2, b"he".to_vec())] {
            let answered = asked(
                &mut systems,
                &json!({"op": "truncate", "path": "/mnt/data/note.txt", "size": size}),
            );
            assert_eq!(
                answered,
                json!({"ok": true}),
                "case {case:?}: a size the file system has room for is performed, got {answered}"
            );
            let read = asked(&mut systems, &json!({"op": "read", "path": "/mnt/data/note.txt"}));
            assert_eq!(
                held(&read),
                Some(expected.clone()),
                "case {case:?}: expected {expected:?}, got {read}"
            );
        }
        // A directory renamed OUTSIDE itself moves with everything under it.
        let moved = asked(
            &mut systems,
            &json!({"op": "rename", "path": "/mnt/data/deep", "target": "/mnt/data/moved"}),
        );
        assert_eq!(
            moved,
            json!({"ok": true}),
            "a directory moves to a path outside itself, got {moved}"
        );
        let read = asked(
            &mut systems,
            &json!({"op": "read", "path": "/mnt/data/moved/held.txt"}),
        );
        assert_eq!(
            held(&read),
            Some(b"deeper".to_vec()),
            "what the directory held moved with it, got {read}"
        );
        let gone = asked(&mut systems, &json!({"op": "list", "path": "/mnt/data"}));
        assert_eq!(
            gone,
            json!({"names": ["moved", "note.txt"]}),
            "the name it moved from is gone, got {gone}"
        );
    }

    /// RULE: an access point whose `RootDirectory` the file system does not hold mounts only
    /// where the access point states the `CreationInfo` to create it with.
    #[test]
    fn a_root_the_file_system_does_not_hold_mounts_only_with_creation_settings() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        let stated = GuestFileSystems::attach(&[mount("/mnt/data", "/held", true, &files)]);
        assert_eq!(
            stated.table()["mounts"].as_array().map(Vec::len),
            Some(1),
            "an access point that states creation settings has its root directory created"
        );
        let mut without = mount("/mnt/data", "/absent", true, &files);
        without.root_creation = None;
        let unmounted = GuestFileSystems::attach(&[without]);
        assert_eq!(
            unmounted.table()["mounts"].as_array().map(Vec::len),
            Some(0),
            "a root the file system does not hold and no settings to create it mounts nothing"
        );
        // The file system's own ROOT is a directory every file system has, so an access point
        // rooted there needs no creation settings.
        let mut whole = mount("/mnt/fs", "/", true, &files);
        whole.root_creation = None;
        let rooted = GuestFileSystems::attach(&[whole]);
        assert_eq!(
            rooted.table()["mounts"].as_array().map(Vec::len),
            Some(1),
            "the file system's own root is a directory it holds"
        );
    }

    /// RULE: the mount table the guest is handed names every mount and the directories above
    /// them, which is what makes `/mnt` a directory of the environment.
    #[test]
    fn the_table_names_the_mounts_and_what_lies_above_them() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        let systems = GuestFileSystems::attach(&[mount("/mnt/data/held", "/shared", true, &files)]);
        let table = systems.table();
        assert_eq!(
            table["mounts"][0]["path"],
            json!("/mnt/data/held"),
            "the mount is at the path the configuration named, got {table}"
        );
        assert_eq!(
            table["mounts"][0]["uid"],
            json!(1001),
            "the access point's own user, got {table}"
        );
        assert_eq!(
            table["above"],
            json!(["/mnt/data", "/mnt"]),
            "every directory above the mount point, deepest first, got {table}"
        );
        assert!(
            GuestFileSystems::none().table()["mounts"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "a function that configures no file system mounts nothing"
        );
    }

    /// RULE: the readings a filesystem answers are the file system's own: the entry's kind and
    /// size, and a `statvfs` of the capacity this emulator holds for it.
    #[test]
    fn the_readings_are_the_file_systems_own() {
        let files = super::efs::MountedFileSystem::empty_for_tests();
        let mut systems = GuestFileSystems::attach(&[mount("/mnt/data", "/shared", true, &files)]);
        asked(&mut systems, &write("/mnt/data/note.txt", b"twelve bytes"));
        asked(&mut systems, &json!({"op": "mkdir", "path": "/mnt/data/deep"}));
        let stat = asked(&mut systems, &json!({"op": "stat", "path": "/mnt/data/note.txt"}));
        assert_eq!(
            stat["entry"]["kind"],
            json!("file"),
            "the entry is a file, got {stat}"
        );
        assert_eq!(
            stat["entry"]["size"],
            json!(12),
            "its size is the bytes it holds, got {stat}"
        );
        let listed = asked(&mut systems, &json!({"op": "list", "path": "/mnt/data"}));
        assert_eq!(
            listed,
            json!({"names": ["deep", "note.txt"]}),
            "the directory holds what was written there, got {listed}"
        );
        let reading = asked(&mut systems, &json!({"op": "statvfs", "path": "/mnt/data"}));
        assert_eq!(
            reading["blockSize"],
            json!(BLOCK_SIZE),
            "the block size the filesystem reports, got {reading}"
        );
        assert!(
            reading["free"].as_u64().is_some_and(|free| free > 0),
            "a file system holding twelve bytes has room left, got {reading}"
        );
    }
}
