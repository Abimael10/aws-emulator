// SPDX-License-Identifier: CC0-1.0

//! The FILESYSTEM one container's program runs over: the container's own writable layer, and
//! the task-definition volumes mounted into it.
//!
//! # The rule this module implements
//!
//! A container runs on a filesystem. Its own layer is writable and lives as long as the
//! container does — ONE layer, shared by every session that runs INSIDE that container
//! ([`ContainerLayer`]), so what the container's own program writes there is what a later
//! session of the same container reads back — and on top of it the agent MOUNTS the volumes
//! the task definition declares: `MountPoint$sourceVolume` is "the name of the volume to mount", `containerPath`
//! "the path on the container to mount the host volume at", and `readOnly` — "if this value
//! is `true`, the container has read-only access to the volume". So inside the container that
//! path IS a filesystem: a redirection into it creates a file, a utility reads the bytes back,
//! and a mount declared read-only refuses every write the way the kernel does, with EROFS,
//! while a read through the same mount still succeeds.
//!
//! WHICH files a mount shows is the volume's own, never a copy: two containers that mount the
//! same volume share one namespace, which is what makes a volume the thing data is handed
//! over through. Where those files live is [`super::container_volumes`]' to resolve — an
//! `efsVolumeConfiguration` volume's are the EFS file system's, held by the service that owns
//! it — and this module is what a program REACHES them through.
//!
//! # What a path resolves to
//!
//! A path is resolved the way a process resolves one: a relative path against the container's
//! working directory (`ContainerDefinition$workingDirectory`, "the working directory to run
//! commands inside the container in"), and `.`/`..` segments resolved before anything else is
//! decided — so a path cannot climb out of the mount it names into the volume's own parent.
//! The mount it lands in is the one with the LONGEST `containerPath` the path lies under,
//! which is how a mount inside another mount's directory shadows it; a path under no mount at
//! all is the container's own layer.
//!
//! What the two differ in is what a MISSING directory means, which is stated on
//! [`ContainerFilesystem::write`]: inside a mount the volume's namespace is wholly known, so a
//! write into a directory it does not hold fails as it does on a container, while the
//! container's own layer sits over a filesystem the image provides and this emulator holds
//! none of.
//!
//! # Bounds
//!
//! The container's layer is bounded by [`CONTAINER_LAYER_LIMITS`] and each volume by the
//! service that holds it. A write past a bound does not happen and is reported to the program
//! as the failure it is ([`FileError::NoSpace`]), so a container's program cannot grow this
//! process without a stated ceiling. No host path is opened, created or read: every entry is
//! one this process holds in memory.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ::service_com_amazonaws_efs_magnolioapiservice_v20150201::state::MountedFileSystem;
use ::service_common::posix_shell_files::{FileEntry, FileRefusal, ShellFiles};
use ::service_common::posix_shell_volume::{
    parent_path, resolved_path, ShellVolume, VolumeLimits, ROOT,
};

/// How much ONE container's own writable layer holds. Every field is the EMULATOR'S number,
/// not a modelled quota (a task's `ephemeralStorage` is sized in gibibytes): the layer is this
/// process's memory, so what a container's program may leave in it is bounded here. A volume
/// mounted into the container is bounded by whoever holds the volume instead.
pub const CONTAINER_LAYER_LIMITS: VolumeLimits = VolumeLimits {
    files: 256,
    bytes: 256 * 1024,
    path_bytes: 4096,
    directories: 256,
    device_lines: 0,
    device_line_bytes: 0,
};

/// How much ONE volume this emulator holds itself may hold — the volumes that are not another
/// service's file system ([`super::container_volumes`]). The EMULATOR'S number, under
/// [`CONTAINER_LAYER_LIMITS`]' rule applied to a volume rather than to a layer.
pub const TASK_VOLUME_LIMITS: VolumeLimits = VolumeLimits {
    files: 512,
    bytes: 1024 * 1024,
    path_bytes: 4096,
    directories: 512,
    device_lines: 0,
    device_line_bytes: 0,
};

/// The path the null device is at, which every container has: a write to it is discarded, as
/// the device is specified to.
const NULL_DEVICE: &str = "/dev/null";

/// The directory a container's program is in when its definition names none, which is the
/// root of its own filesystem.
pub const DEFAULT_WORKING_DIRECTORY: &str = ROOT;

/// The user and group a process of a container acts as when nothing narrows it, which is what
/// a listing reports for the files it creates.
const DEFAULT_OWNER: &str = "root";

/// A handle onto the writable layer of ONE container, shared by every session that runs
/// inside that container: the container's own program, and the health-check probes the agent
/// runs in it ([`super::container_health`]). A container has one filesystem, so a file the
/// program created is a file a probe of the same container finds — and one the program
/// removed is one it no longer finds.
#[derive(Debug, Clone)]
pub struct ContainerLayer {
    files: Arc<Mutex<ShellVolume>>,
}

impl Default for ContainerLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerLayer {
    /// The layer of a container nothing has run in yet: the null device every container has,
    /// and nothing else.
    #[must_use]
    pub fn new() -> Self {
        let mut layer = ShellVolume::new(CONTAINER_LAYER_LIMITS);
        layer.add_discarding_device(NULL_DEVICE);
        Self {
            files: Arc::new(Mutex::new(layer)),
        }
    }

    /// The tree, with a lock poisoned by an earlier panic taken rather than propagated: the
    /// layer outlives the one session that was writing to it, so one failed write does not
    /// take the container's files with it.
    fn files(&self) -> MutexGuard<'_, ShellVolume> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many bytes this layer holds — the sum of what every file on it holds, which is
    /// memory this process is really holding for the container
    /// ([`super::container_consumption`]). The same sum [`ContainerFilesystem::layer_bytes`]
    /// reports for a session running over it.
    pub fn held_bytes(&self) -> u64 {
        self.files()
            .stored_files()
            .map(|(_, contents)| u64::try_from(contents.len()).unwrap_or(u64::MAX))
            .sum()
    }
}

/// A handle onto the files ONE volume this emulator holds itself holds, shared by every
/// container that mounts it: what one container writes through its handle, another reads
/// through its own.
#[derive(Debug, Clone)]
pub struct VolumeFiles {
    files: Arc<Mutex<ShellVolume>>,
}

impl VolumeFiles {
    /// An empty volume — one nothing has written to yet.
    pub(super) fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(ShellVolume::new(TASK_VOLUME_LIMITS))),
        }
    }

    /// The tree, with a lock poisoned by an earlier panic taken rather than propagated: a
    /// volume outlives the container that was writing to it, so one failed write does not take
    /// the volume's data with it.
    fn files(&self) -> MutexGuard<'_, ShellVolume> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where the files of one mount LIVE: a file system of the service that owns it, or storage
/// this emulator holds itself.
#[derive(Debug, Clone)]
pub enum MountedFiles {
    /// An Amazon EFS file system, whose files are held by that service beside the file system
    /// record they belong to.
    FileSystem(MountedFileSystem),
    /// A volume of this service's own store, held for the task or the container instance the
    /// volume's configuration says its data belongs to.
    Volume(VolumeFiles),
}

impl MountedFiles {
    /// The one tree this mount shows, whoever holds it.
    fn files(&self) -> MutexGuard<'_, ShellVolume> {
        match self {
            Self::FileSystem(file_system) => file_system.files(),
            Self::Volume(volume) => volume.files(),
        }
    }
}

/// ONE volume mounted into a container, as the container's own filesystem holds it.
#[derive(Debug, Clone)]
pub struct ContainerMount {
    /// `MountPoint$containerPath`: the path inside the container the volume is mounted at,
    /// resolved and with no trailing separator.
    container_path: String,
    /// The path INSIDE the volume's own tree that `container_path` shows — an access point's
    /// own root directory, or the `rootDirectory` the volume configuration named.
    source_root: String,
    /// `MountPoint$readOnly`: whether the container has read-only access to the volume.
    read_only: bool,
    /// The user and group a file of this mount belongs to, which an access point's
    /// `PosixUser` is what narrows.
    owner: FileOwner,
    /// Where the files are.
    files: MountedFiles,
}

/// The user and group a filesystem entry belongs to, as a listing of it reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOwner {
    /// The owning user: a name where the container has one, and otherwise the id itself,
    /// which is what a listing reports for an id no name is held for.
    pub user: String,
    /// The owning group, read the same way.
    pub group: String,
}

impl Default for FileOwner {
    fn default() -> Self {
        Self {
            user: DEFAULT_OWNER.to_string(),
            group: DEFAULT_OWNER.to_string(),
        }
    }
}

impl FileOwner {
    /// The owner a POSIX user id and group id name, which is how a listing reports an
    /// identity the container holds no name for.
    pub fn of_ids(uid: i64, gid: i64) -> Self {
        Self {
            user: uid.to_string(),
            group: gid.to_string(),
        }
    }
}

impl ContainerMount {
    /// The mount of `files` at `container_path`, rooted at `source_root` inside that volume,
    /// read-only when `read_only`, whose files belong to `owner`.
    ///
    /// The volume's root directory is MADE as the mount is attached: a file system's own root
    /// exists, and an access point's "root directory ... is created" when the access point is
    /// used, so the directory a mount is rooted at is a directory from the mount on.
    pub fn attach(
        container_path: &str,
        source_root: &str,
        read_only: bool,
        owner: FileOwner,
        files: MountedFiles,
    ) -> Option<Self> {
        let container_path = resolved_path(container_path)?;
        let source_root = resolved_path(source_root)?;
        if !files.files().make_directories(&source_root) {
            // The volume will not hold the directory the mount is rooted at, so there is
            // nothing to mount: reported as no mount at all rather than as an empty one.
            return None;
        }
        Some(Self {
            container_path,
            source_root,
            read_only,
            owner,
            files,
        })
    }

    /// The path inside the volume that `path` — already resolved, inside this mount — names.
    fn source_of(&self, path: &str) -> String {
        let relative = path
            .strip_prefix(&self.container_path)
            .unwrap_or_default()
            .trim_start_matches('/');
        if relative.is_empty() {
            return self.source_root.clone();
        }
        if self.source_root.ends_with('/') {
            return format!("{}{relative}", self.source_root);
        }
        format!("{}/{relative}", self.source_root)
    }

    /// Whether the resolved `path` lies in this mount — the mount point itself, or anything
    /// under it.
    fn holds(&self, path: &str) -> bool {
        if self.container_path == ROOT {
            return true;
        }
        match path.strip_prefix(self.container_path.as_str()) {
            Some("") => true,
            // Only a SEPARATOR after the mount point means the path is under it: `/datax` is
            // not inside the mount at `/data`.
            Some(under) => under.starts_with('/'),
            None => false,
        }
    }
}

/// The filesystem ONE container's program runs over. See the module docs.
#[derive(Debug)]
pub struct ContainerFilesystem {
    /// The container's OWN writable layer: what it writes outside every mount, which lives
    /// as long as the container does and is shared by every session that runs in it
    /// ([`ContainerLayer`]).
    layer: ContainerLayer,
    /// The volumes mounted into it, LONGEST `containerPath` first, so the mount a path lands
    /// in is the deepest one holding it.
    mounts: Vec<ContainerMount>,
    /// `ContainerDefinition$workingDirectory`, which a relative path is resolved against.
    working_directory: String,
}

impl Default for ContainerFilesystem {
    fn default() -> Self {
        Self::new(None, Vec::new())
    }
}

impl ContainerFilesystem {
    /// The filesystem of a container whose definition names `working_directory`, with
    /// `mounts` attached.
    ///
    /// The layer starts as a container's does: the null device, and the directory each mount
    /// point is at — a mount point is a directory of the container's own filesystem that the
    /// volume is mounted over, so the path above a mount is one the program can list.
    pub fn new(working_directory: Option<&str>, mounts: Vec<ContainerMount>) -> Self {
        Self::on_layer(ContainerLayer::new(), working_directory, mounts)
    }

    /// The filesystem of the container whose own writable layer is `layer`, with `mounts`
    /// attached and `working_directory` as the directory a relative path is resolved against.
    ///
    /// Every session that runs INSIDE one container is given that container's layer, so the
    /// files one of them leaves there are the files the next one sees: the container's own
    /// program writes its page and its marker files, and the agent's health-check probe reads
    /// exactly those ([`super::container_health`]).
    pub fn on_layer(
        layer: ContainerLayer,
        working_directory: Option<&str>,
        mut mounts: Vec<ContainerMount>,
    ) -> Self {
        {
            let mut files = layer.files();
            for mount in &mounts {
                let _ = files.make_directories(&mount.container_path);
            }
        }
        // Longest first: a path is matched against the deepest mount that holds it.
        mounts.sort_by(|left, right| {
            right
                .container_path
                .len()
                .cmp(&left.container_path.len())
                .then_with(|| left.container_path.cmp(&right.container_path))
        });
        let working_directory = working_directory
            .map(str::trim)
            .filter(|directory| directory.starts_with('/'))
            .unwrap_or(DEFAULT_WORKING_DIRECTORY)
            .to_string();
        Self {
            layer,
            mounts,
            working_directory,
        }
    }

    /// `ContainerDefinition$workingDirectory`, "the working directory to run commands inside
    /// the container in", which is also the directory a server the program starts serves when
    /// the invocation names none ([`super::container_servers`]).
    #[must_use]
    pub fn working_in(&self) -> &str {
        &self.working_directory
    }

    /// Whether this container has any volume mounted into it at all.
    pub fn has_mounts(&self) -> bool {
        !self.mounts.is_empty()
    }

    /// Whether `path` lands inside a volume MOUNTED into this container, as against on the
    /// container's own writable layer: the two are different filesystems, and what is known
    /// about each of them is different ([`super::container_process`]).
    pub fn is_in_a_mount(&self, path: &str) -> bool {
        self.locate(path)
            .is_some_and(|located| located.mount.is_some())
    }

    /// How many bytes the container's own writable LAYER holds — the sum of what every file
    /// on it holds, which is what the space on that filesystem is reported as used
    /// ([`super::container_process`]).
    pub fn layer_bytes(&self) -> u64 {
        self.layer.held_bytes()
    }

    /// `path` as a process of this container resolves it: a relative path against the
    /// container's working directory, with `.` and `..` segments resolved — and `None` for a
    /// path no filesystem here accepts.
    fn resolve(&self, path: &str) -> Option<String> {
        if path.starts_with('/') {
            return resolved_path(path);
        }
        if self.working_directory.ends_with('/') {
            return resolved_path(&format!("{}{path}", self.working_directory));
        }
        resolved_path(&format!("{}/{path}", self.working_directory))
    }

    /// Which store holds `path`, and the path inside that store — the mount it lands in, or
    /// the container's own layer.
    fn locate(&self, path: &str) -> Option<Located> {
        let resolved = self.resolve(path)?;
        for (index, mount) in self.mounts.iter().enumerate() {
            if mount.holds(&resolved) {
                return Some(Located {
                    mount: Some(index),
                    path: mount.source_of(&resolved),
                });
            }
        }
        Some(Located {
            mount: None,
            path: resolved,
        })
    }

    /// Run `read` over the tree `path` lives in, with the path inside that tree.
    fn in_tree<T>(&self, path: &str, read: impl FnOnce(&ShellVolume, &str) -> T) -> Option<T> {
        let located = self.locate(path)?;
        match located.mount {
            Some(index) => {
                let mount = self.mounts.get(index)?;
                let files = mount.files.files();
                Some(read(&files, &located.path))
            }
            None => {
                let files = self.layer.files();
                Some(read(&files, &located.path))
            }
        }
    }

    /// Run `write` over the tree `path` lives in, unless that tree is mounted read-only.
    fn in_writable_tree<T>(
        &mut self,
        path: &str,
        write: impl FnOnce(&mut ShellVolume, &str) -> T,
    ) -> Result<T, FileRefusal> {
        let located = self.locate(path).ok_or(FileRefusal::NotFound)?;
        match located.mount {
            Some(index) => {
                let mount = self.mounts.get(index).ok_or(FileRefusal::NotFound)?;
                if mount.read_only {
                    // "If this value is `true`, the container has read-only access to the
                    // volume": the kernel refuses every write to it.
                    return Err(FileRefusal::ReadOnly);
                }
                let mut files = mount.files.files();
                Ok(write(&mut files, &located.path))
            }
            None => {
                let mut files = self.layer.files();
                Ok(write(&mut files, &located.path))
            }
        }
    }
}

/// The container's filesystem AS A SESSION REACHES IT
/// ([`::service_common::posix_shell_files`]): what the utilities a program names work over, so
/// `cat`, `ls`, `mkdir`, `touch` and `rm` are the one reading of those utilities this workspace
/// has, performed over a container's own layer and its mounts.
impl ShellFiles for ContainerFilesystem {
    /// What reading `path` answers, or the error a read of it fails with: nothing at that
    /// path, or a directory, which is not something to read bytes out of.
    fn read(&self, path: &str) -> Result<String, FileRefusal> {
        if self.is_directory(path) {
            return Err(FileRefusal::IsADirectory);
        }
        self.in_tree(path, |files, path| files.read(path))
            .flatten()
            .ok_or(FileRefusal::NotFound)
    }

    /// Write `text` at `path`, replacing what is there or APPENDING to it — the two forms an
    /// output redirection opens a file in.
    ///
    /// The path is resolved before anything else is decided, which is the order the kernel
    /// decides them in: a directory is [`FileRefusal::IsADirectory`] and a path whose directory
    /// is a FILE is [`FileRefusal::NotADirectory`] whether or not the mount is read-only, a
    /// mount that is read-only then refuses the write with [`FileRefusal::ReadOnly`], and a
    /// tree that will hold no more answers [`FileRefusal::NoSpace`].
    ///
    /// WHERE the write lands decides what a MISSING directory above it means. Inside a mount
    /// the namespace is the volume's own and wholly known, so a write whose directory the
    /// volume does not hold fails with [`FileRefusal::NotFound`], exactly as it does on a
    /// container. In the container's own layer it does not: that layer sits over the
    /// filesystem the container's IMAGE provides, which this emulator holds none of, so it
    /// cannot say that a directory the image provides is missing — the directories above such
    /// a write are made with it. Nothing is invented for a READ: a path nothing wrote is still
    /// a path this filesystem holds nothing at.
    fn write(&mut self, path: &str, text: &str, append: bool) -> Result<(), FileRefusal> {
        if self.is_directory(path) {
            return Err(FileRefusal::IsADirectory);
        }
        let located = self.locate(path).ok_or(FileRefusal::NotFound)?;
        let resolved = self.resolve(path).ok_or(FileRefusal::NotFound)?;
        if let Some(parent) = parent_path(&resolved) {
            if self.is_file(&parent) {
                return Err(FileRefusal::NotADirectory);
            }
            if !self.is_directory(&parent) {
                if located.mount.is_some() {
                    return Err(FileRefusal::NotFound);
                }
                self.make_directories(&parent)?;
            }
        }
        let wrote = self.in_writable_tree(path, |files, path| files.write(path, text, append))?;
        if wrote {
            Ok(())
        } else {
            Err(FileRefusal::NoSpace)
        }
    }

    /// Make `path` a directory, with every directory above it — what `mkdir -p` does.
    fn make_directories(&mut self, path: &str) -> Result<(), FileRefusal> {
        if self.is_file(path) {
            return Err(FileRefusal::NotADirectory);
        }
        if self.is_directory(path) {
            return Ok(());
        }
        let made = self.in_writable_tree(path, |files, path| files.make_directories(path))?;
        if made {
            Ok(())
        } else {
            Err(FileRefusal::NoSpace)
        }
    }

    /// Remove the file at `path`.
    fn remove_file(&mut self, path: &str) -> Result<(), FileRefusal> {
        if self.is_directory(path) {
            return Err(FileRefusal::IsADirectory);
        }
        if !self.is_file(path) {
            return Err(FileRefusal::NotFound);
        }
        let removed = self.in_writable_tree(path, |files, path| files.remove_file(path))?;
        if removed {
            Ok(())
        } else {
            Err(FileRefusal::NotFound)
        }
    }

    /// Whether this filesystem holds anything at `path`.
    fn exists(&self, path: &str) -> bool {
        self.in_tree(path, |files, path| files.exists(path))
            .unwrap_or(false)
    }

    /// Whether `path` is a regular file of this filesystem.
    fn is_file(&self, path: &str) -> bool {
        self.in_tree(path, |files, path| files.is_file(path))
            .unwrap_or(false)
    }

    /// Whether `path` is a directory of this filesystem.
    fn is_directory(&self, path: &str) -> bool {
        self.in_tree(path, |files, path| files.is_directory(path))
            .unwrap_or(false)
    }

    /// Whether a write to `path` would be performed at all, which is what `test -w` asks:
    /// false for a path on a read-only mount, one a directory holds, and one inside a mount
    /// whose own directory the volume does not hold.
    fn writable(&self, path: &str) -> bool {
        let Some(located) = self.locate(path) else {
            return false;
        };
        if self.is_directory(path) {
            return false;
        }
        if let Some(parent) = self.resolve(path).as_deref().and_then(parent_path) {
            if self.is_file(&parent) {
                return false;
            }
            // Inside a mount the volume's namespace is wholly known, so a missing directory
            // is a write that would not happen; in the container's own layer it is made with
            // the write ([`Self::write`]).
            if located.mount.is_some() && !self.is_directory(&parent) {
                return false;
            }
        }
        located
            .mount
            .and_then(|index| self.mounts.get(index))
            .is_none_or(|mount| !mount.read_only)
    }

    /// What this filesystem holds at `path`, as a long listing reports it — and `None` for a
    /// path it holds nothing at.
    ///
    /// The owner is the mount's: a file of a volume mounted through an access point belongs to
    /// the identity that access point applies, and one of the container's own layer to the
    /// process that wrote it.
    fn entry(&self, path: &str) -> Option<FileEntry> {
        let located = self.locate(path)?;
        let (facts, owner) = match located.mount {
            Some(index) => {
                let mount = self.mounts.get(index)?;
                let facts = mount.files.files().facts(&located.path)?;
                (facts, mount.owner.clone())
            }
            None => (self.layer.files().facts(&located.path)?, FileOwner::default()),
        };
        Some(FileEntry {
            kind: facts.kind,
            size: facts.size,
            modified: facts.modified,
            user: owner.user,
            group: owner.group,
        })
    }

    /// The names of the entries directly under the directory `path`, sorted — what a listing
    /// of a directory names. Empty for a path that is not a directory.
    fn entry_names(&self, path: &str) -> Vec<String> {
        self.in_tree(path, |files, path| files.entry_names(path))
            .unwrap_or_default()
    }
}

/// Which store a path lives in, and the path inside that store.
#[derive(Debug)]
struct Located {
    /// The index into [`ContainerFilesystem::mounts`] of the mount holding it, and `None` for
    /// the container's own layer.
    mount: Option<usize>,
    /// The path inside that store.
    path: String,
}

