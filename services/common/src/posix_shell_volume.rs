// SPDX-License-Identifier: CC0-1.0

//! The in-memory TREE a bounded shell session reads and writes, for the emulated services
//! whose caller-supplied program is a program of a GUEST.
//!
//! # Why a session needs one
//!
//! A program that runs on a machine writes to that machine: a boot script keeps a counter
//! on the root volume, writes a line to the console device and reads an identity the kernel
//! publishes. A session with nowhere to read or write cannot perform one of those lines at
//! all, so the service that holds the machine (an EC2 instance and its volumes) hands the
//! session this tree, which is the WHOLE of what its commands can reach. There is no host
//! path behind it: every entry is one this process holds in memory, every one of them was
//! put here by the service that owns the machine, and nothing here opens, resolves or lists
//! a path of the host.
//!
//! # What it holds
//!
//! * FILES the program may read and write, which is what the machine's own storage is —
//!   the caller takes them back out ([`ShellVolume::stored_files`]) and keeps them beside
//!   the machine, so what one run wrote the next run reads;
//! * SEALED files, which the service seeded and a program may only READ: the values a
//!   kernel or an agent publishes to a guest rather than values the guest chose;
//! * DEVICES, which a program may only WRITE: a sink that keeps the lines written to it
//!   ([`ShellVolume::device_lines`], which is how a console gets what a boot script wrote)
//!   or one that discards them, the way `/dev/null` does;
//! * DIRECTORIES, so a write to a path whose directory the machine does not have fails the
//!   way it fails on a machine rather than silently inventing the tree.
//!
//! # What a listing of it reports
//!
//! A program asks what the tree HOLDS as well as what is in a file: a long listing names
//! what each entry is, how many bytes it holds and when it was last written
//! ([`ShellVolume::facts`]), and a listing of a directory names the entries directly under
//! it ([`ShellVolume::entry_names`]). Those are facts of the tree itself — the entry's own
//! kind, the bytes it holds and the instant a write of it happened — so they are kept here
//! beside the contents rather than computed by whoever reports them; how a caller WORDS a
//! listing is the caller's, because the form belongs to the utility it stands for.
//!
//! # What is MOUNTED into it
//!
//! Not everything a program reaches through this tree is the machine's own storage: a
//! filesystem of ANOTHER component — a network filesystem whose files are held beside the
//! resource that owns them — is MOUNTED at a directory of this tree
//! ([`ShellVolume::mount`]), and from then on every path under that directory is a path of
//! that filesystem instead. The mounted tree is SHARED, not copied: what a program on one
//! machine writes under the mount point, a program on another machine that mounted the same
//! filesystem reads under its own, which is the whole of what mounting one is for. Taking it
//! away again ([`ShellVolume::unmount`]) leaves the directory it was mounted over exactly as
//! it was, and leaves the filesystem's own files with whoever holds them: they were never
//! this machine's storage, so they are not among the files the caller keeps beside the
//! machine ([`ShellVolume::stored_files`]).
//!
//! What a mount REPORTS about itself — the filesystem type a listing of the mounts names, the
//! type a statfs of a path on it names, what was mounted and with which options
//! ([`MountFacts`]) — is the mounting component's to state, and is kept beside the mount so
//! the programs that ask about it (a guest's `findmnt`, its `stat -f`) answer out of the one
//! mount table rather than out of a second notion of what is mounted where.
//!
//! # Bounds
//!
//! Every write is bounded by [`VolumeLimits`], held here rather than in the program's
//! reach: how many files the tree holds and how many bytes they come to, how long one path
//! may be, and how many lines and bytes one device keeps. A write past a bound does not
//! happen and is reported as the failure it is, so a caller-supplied program cannot grow
//! this process without a stated ceiling. A write that lands in a MOUNTED filesystem is
//! bounded by that filesystem's own limits, held by whoever holds its files.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

/// How much one volume may hold. Every field is the EMULATOR's number: the programs are
/// caller-supplied text, so what one of them can store is bounded here.
#[derive(Debug, Clone, Copy)]
pub struct VolumeLimits {
    /// How many files the tree holds at once, sealed ones included.
    pub files: usize,
    /// How many bytes those files' paths and contents come to at once.
    pub bytes: usize,
    /// The longest path this tree accepts.
    pub path_bytes: usize,
    /// How many directories the tree holds at once.
    pub directories: usize,
    /// How many lines ONE device keeps. Past it a write is refused rather than kept.
    pub device_lines: usize,
    /// How many bytes ONE line written to a device may carry. A longer line is truncated on
    /// a character boundary, so what the program wrote is still reported.
    pub device_line_bytes: usize,
}

impl Default for VolumeLimits {
    /// Bounds far above any boot script a guest carries and far below what would cost this
    /// process anything noticeable.
    fn default() -> Self {
        Self {
            files: 256,
            bytes: 256 * 1024,
            path_bytes: 1024,
            directories: 256,
            device_lines: 512,
            device_line_bytes: 4 * 1024,
        }
    }
}

/// What one DEVICE of the tree does with the lines written to it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Device {
    /// Keep them, up to the volume's bounds — a console, whose whole purpose is that
    /// something reads back what was written to it.
    Keep(Vec<String>),
    /// Discard them, which is the whole of what the null device does.
    Discard,
}

/// What one entry of a tree IS — which is the first thing a long listing of it reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file: something with contents a program reads and writes.
    File,
    /// A directory: something that holds the names of the entries under it.
    Directory,
    /// A device, which a program writes to rather than keeps bytes in.
    Device,
}

/// One entry of a tree as a long listing reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryFacts {
    /// What the entry is.
    pub kind: EntryKind,
    /// How many bytes it holds: a file's contents, and for a directory the names it holds,
    /// which is what a directory's own bytes are.
    pub size: usize,
    /// When this entry was last written — a write of the entry itself, and for a directory
    /// the creation or removal of something under it, which is what changes a directory.
    pub modified: SystemTime,
}

/// What a MOUNTED filesystem reports about itself, as the programs that ask about the mounts
/// of a machine report it.
///
/// Every field is the MOUNTING component's statement about the filesystem it mounted, kept
/// beside the mount so a guest's `findmnt` and its `stat -f` answer out of the one mount
/// table: the type a mount listing names a filesystem by, the type a statfs of a path on it
/// names (the two differ — a network filesystem mounted as `nfs4` is a `nfs` filesystem to
/// statfs), what was mounted, and the options it was mounted with as the command wrote them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountFacts {
    /// The filesystem TYPE a listing of the mounts reports — `findmnt`'s own `FSTYPE`.
    pub file_system_type: String,
    /// The type a statfs of a path on this filesystem reports — `stat -f`'s own `%T`.
    pub statfs_type: String,
    /// WHAT was mounted, as the mounting command's first operand named it — `findmnt`'s own
    /// `SOURCE`.
    pub source: String,
    /// The options the filesystem was mounted with, as the command wrote them —
    /// `findmnt`'s own `OPTIONS`.
    pub options: String,
}

/// A tree ANOTHER component holds, as a volume mounts it: the files themselves, the path
/// INSIDE them the mount point shows, and what the mount reports about itself.
///
/// The files are shared rather than copied — that is what makes a mount the thing data is
/// handed over through — so two machines that mount the same filesystem reach one tree, and a
/// lock an earlier panic poisoned is taken rather than propagated: a filesystem outlives the
/// program that was writing to it.
#[derive(Debug, Clone)]
pub struct MountedTree {
    /// The one tree the mounted filesystem holds.
    files: Arc<Mutex<ShellVolume>>,
    /// The path inside that tree the mount point shows, resolved — the whole filesystem for
    /// a mount of its root, and a directory of it for a mount rooted deeper.
    root: String,
    /// What the mount reports about itself.
    facts: MountFacts,
}

impl MountedTree {
    /// The mount of `files` rooted at `root` inside them, reporting `facts` — `None` for a
    /// root that is not an absolute path at all.
    #[must_use]
    pub fn new(files: Arc<Mutex<ShellVolume>>, root: &str, facts: MountFacts) -> Option<Self> {
        Some(Self {
            files,
            root: resolved_path(root)?,
            facts,
        })
    }

    /// What this mount reports about itself.
    #[must_use]
    pub fn facts(&self) -> &MountFacts {
        &self.facts
    }

    /// Whether the mounted filesystem is one this tree can mount AT ALL, and holds the
    /// directory this mount is rooted at — what makes it something to mount: a filesystem that
    /// holds nothing there would show an empty tree at the mount point rather than the
    /// directory the mount names, and one that already MOUNTS something itself would make the
    /// mount a chain rather than one filesystem at one directory.
    ///
    /// Asked WITHOUT waiting for the mounted tree's lock ([`Self::unlocked_files`]): the only
    /// way that lock is already held at this point is that the filesystem being mounted is the
    /// one being mounted INTO, or one reached through it, which is a mount this tree will not
    /// make rather than one it blocks for — a lock of this kind is not reentrant, so waiting
    /// for it would wedge the thread for good. A filesystem something else is working on at
    /// this instant is answered the same way, and the command that asked for the mount reports
    /// the failure as its own.
    #[must_use]
    pub fn can_be_mounted(&self) -> bool {
        self.unlocked_files()
            .is_some_and(|files| files.mounts.is_empty() && files.is_directory(&self.root))
    }

    /// The mounted tree, with a lock poisoned by an earlier panic taken rather than
    /// propagated.
    ///
    /// LOCK ORDER: the mounting tree is held before the mounted one, never the other way
    /// round, and a mounted tree carries no mounts of its own ([`Self::can_be_mounted`]) — so
    /// delegating one path operation takes exactly these two locks and no third.
    fn files(&self) -> MutexGuard<'_, ShellVolume> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// [`Self::files`] for a caller that must not WAIT for that lock, answering `None` when
    /// something holds it at this instant — which is what makes a mount of a filesystem into
    /// itself a refusal instead of a thread that never returns.
    fn unlocked_files(&self) -> Option<MutexGuard<'_, ShellVolume>> {
        match self.files.try_lock() {
            Ok(files) => Some(files),
            Err(::std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(::std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

/// ONE filesystem mounted into a tree, as that tree holds it.
#[derive(Debug, Clone)]
struct Mount {
    /// The directory of the mounting tree the filesystem is mounted at, resolved.
    point: String,
    /// The filesystem mounted there.
    tree: MountedTree,
}

impl Mount {
    /// Whether the resolved `path` lies in this mount — the mount point itself, or anything
    /// under it. Only a SEPARATOR after the mount point puts a path inside it: `/datax` is
    /// not inside the mount at `/data`.
    fn holds(&self, path: &str) -> bool {
        match path.strip_prefix(self.point.as_str()) {
            Some("") => true,
            Some(under) => under.starts_with('/'),
            None => false,
        }
    }

    /// The path INSIDE the mounted filesystem that the resolved `path` names.
    fn source_of(&self, path: &str) -> String {
        let relative = path
            .strip_prefix(self.point.as_str())
            .unwrap_or_default()
            .trim_start_matches('/');
        if relative.is_empty() {
            return self.tree.root.clone();
        }
        if self.tree.root.ends_with('/') {
            return format!("{}{relative}", self.tree.root);
        }
        format!("{}/{relative}", self.tree.root)
    }
}

/// The tree one session's commands read and write. See the module docs.
#[derive(Debug, Clone)]
pub struct ShellVolume {
    /// The files a program may read and write, by absolute path.
    files: BTreeMap<String, String>,
    /// The paths of [`Self::files`] entries a program may only READ.
    sealed: BTreeSet<String>,
    /// The directories the tree has, by absolute path, always including the root.
    directories: BTreeSet<String>,
    /// The devices the tree has, by absolute path.
    devices: BTreeMap<String, Device>,
    /// When each entry of the tree was last written, by absolute path — what
    /// [`EntryFacts::modified`] reports. An entry is recorded here as it is written, so the
    /// instant is the one the write happened at rather than one computed afterwards.
    modified: BTreeMap<String, SystemTime>,
    /// How many bytes the files come to, kept as they are written rather than recomputed.
    held_bytes: usize,
    /// The filesystems of other components mounted into this tree, DEEPEST mount point
    /// first, so a path lands in the deepest mount that holds it — which is how a mount
    /// inside another mount's directory shadows it.
    mounts: Vec<Mount>,
    limits: VolumeLimits,
}

impl Default for ShellVolume {
    fn default() -> Self {
        Self::new(VolumeLimits::default())
    }
}

/// The directory every tree has, which every absolute path is resolved against.
pub const ROOT: &str = "/";

/// `path` with the empty, `.` and `..` segments a path may carry resolved, as a process
/// resolves an ABSOLUTE path — and `None` for a path that is not absolute at all, which a
/// caller resolves against its own working directory first.
///
/// `..` above the root resolves to the root, as it does on a machine, which is also what keeps
/// a path under a mount point from climbing out of what is mounted there.
///
/// The one reading of that rule in this workspace: [`ShellVolume::resolve`] is this with the
/// tree's own path bound applied, and a caller that has to resolve a path BEFORE it knows
/// which tree holds it calls this.
#[must_use]
pub fn resolved_path(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            named => segments.push(named),
        }
    }
    if segments.is_empty() {
        return Some(ROOT.to_string());
    }
    Some(format!("/{}", segments.join("/")))
}

/// The directory the resolved `path` lies in, and `None` for a path with no directory above
/// it — the ROOT itself, which lies in no directory.
#[must_use]
pub fn parent_path(path: &str) -> Option<String> {
    let (parent, _) = path.trim_end_matches('/').rsplit_once('/')?;
    if parent.is_empty() {
        return Some(ROOT.to_string());
    }
    Some(parent.to_string())
}

impl ShellVolume {
    /// An empty tree — the root directory and nothing else — bounded by `limits`.
    #[must_use]
    pub fn new(limits: VolumeLimits) -> Self {
        Self {
            files: BTreeMap::new(),
            sealed: BTreeSet::new(),
            directories: BTreeSet::from([ROOT.to_string()]),
            devices: BTreeMap::new(),
            modified: BTreeMap::from([(ROOT.to_string(), SystemTime::now())]),
            held_bytes: 0,
            mounts: Vec::new(),
            limits,
        }
    }

    /// Mount `tree` at `path`, so every path under it is a path of that filesystem from now
    /// on — what mounting a filesystem onto a directory of a machine does.
    ///
    /// `false` is a mount that did not happen, which the command performing it reports as its
    /// own failure: a path this tree does not accept, a path it holds no DIRECTORY of its own
    /// at (a filesystem is mounted over a directory, and one inside another mount is not this
    /// tree's to mount over), a point something is already mounted at, and a filesystem that
    /// is not one this tree can mount — one that does not hold the directory the mount would
    /// be rooted at, one that already mounts something itself, and this tree itself or any
    /// tree reached through it ([`MountedTree::can_be_mounted`], which is also what keeps one
    /// path operation to two locks and makes a mount of a filesystem into itself a refusal
    /// rather than a thread that never returns).
    pub fn mount(&mut self, path: &str, tree: MountedTree) -> bool {
        let Some(point) = self.resolve(path) else {
            return false;
        };
        if !self.directories.contains(&point) {
            return false;
        }
        if self.mounts.iter().any(|mount| mount.point == point) {
            return false;
        }
        if !tree.can_be_mounted() {
            return false;
        }
        self.mounts.push(Mount { point, tree });
        // Deepest first: a path is matched against the deepest mount that holds it.
        self.mounts.sort_by(|left, right| {
            right
                .point
                .len()
                .cmp(&left.point.len())
                .then_with(|| left.point.cmp(&right.point))
        });
        true
    }

    /// Take the filesystem mounted AT `path` away again, answering whether one was mounted
    /// there — `false` being what a command unmounting a path nothing is mounted at reports.
    ///
    /// The directory the filesystem was mounted over is left exactly as it was, and the
    /// filesystem's own files are left with whoever holds them.
    pub fn unmount(&mut self, path: &str) -> bool {
        let Some(point) = self.resolve(path) else {
            return false;
        };
        let held = self.mounts.len();
        self.mounts.retain(|mount| mount.point != point);
        self.mounts.len() < held
    }

    /// What is mounted AT `path` exactly, and `None` for a path that is not a mount point of
    /// this tree — what a listing of the mounts answers for a path it was given.
    #[must_use]
    pub fn mount_facts(&self, path: &str) -> Option<&MountFacts> {
        let resolved = self.resolve(path)?;
        self.mounts
            .iter()
            .find(|mount| mount.point == resolved)
            .map(|mount| mount.tree.facts())
    }

    /// What filesystem the resolved `path` lies ON — the deepest mount holding it — and
    /// `None` for a path on the tree's own storage, whose filesystem is the machine's and not
    /// something mounted into it.
    #[must_use]
    pub fn file_system_facts(&self, path: &str) -> Option<&MountFacts> {
        let resolved = self.resolve(path)?;
        self.mounts
            .iter()
            .find(|mount| mount.holds(&resolved))
            .map(|mount| mount.tree.facts())
    }

    /// Read the tree the resolved `path` lies in, when that is a MOUNTED filesystem rather
    /// than this tree's own storage — with the path inside that filesystem.
    ///
    /// LOCK ORDER: the mounted tree's lock is taken while this tree's caller holds whatever it
    /// holds, and a mounted tree carries no mounts of its own ([`MountedTree::can_be_mounted`]),
    /// so this takes exactly one further lock and never a third.
    fn in_mount<T>(&self, resolved: &str, read: impl FnOnce(&ShellVolume, &str) -> T) -> Option<T> {
        let mount = self.mounts.iter().find(|mount| mount.holds(resolved))?;
        let inside = mount.source_of(resolved);
        let files = mount.tree.files();
        Some(read(&files, &inside))
    }

    /// [`Self::in_mount`] for a change of that filesystem. It takes `&self`: what is being
    /// changed is the MOUNTED tree, which is shared and reached through its own lock, and not
    /// this tree.
    fn in_mount_mut<T>(
        &self,
        resolved: &str,
        write: impl FnOnce(&mut ShellVolume, &str) -> T,
    ) -> Option<T> {
        let mount = self.mounts.iter().find(|mount| mount.holds(resolved))?;
        let inside = mount.source_of(resolved);
        let mut files = mount.tree.files();
        Some(write(&mut files, &inside))
    }

    /// `path` as this tree resolves it: absolute, with the empty, `.` and `..` segments a
    /// path may carry resolved ([`resolved_path`]), or `None` for a path this tree does not
    /// accept — a relative one (a session has no working directory of its own), or one past
    /// [`VolumeLimits::path_bytes`].
    #[must_use]
    pub fn resolve(&self, path: &str) -> Option<String> {
        if path.len() > self.limits.path_bytes {
            return None;
        }
        resolved_path(path)
    }

    /// The directory `path` lies in, or `None` for the root itself.
    fn parent_of(path: &str) -> Option<String> {
        parent_path(path)
    }

    /// Seed a file a program may read and WRITE, with the directories above it — what the
    /// machine's storage already held when this run started.
    ///
    /// `false` when the tree is at its bounds and the file was not seeded.
    ///
    /// A seed is the MACHINE's own storage and is MOUNT-BLIND, as the devices below are: the
    /// file is put in this tree, so a path that lies under a mount point is shadowed by what
    /// is mounted there, exactly as the machine's own file under a mount point is on a machine.
    /// A service seeds before a program runs and mounts happen while it runs, so the two do
    /// not meet in practice.
    pub fn put_file(&mut self, path: &str, contents: &str) -> bool {
        let Some(resolved) = self.resolve(path) else {
            return false;
        };
        self.make_directories_of(&resolved);
        self.write_resolved(&resolved, contents, false)
    }

    /// Seed a file a program may only READ: a value the machine publishes to its guest
    /// rather than one the guest chose. A write to it fails the way a write to a read-only
    /// file fails.
    ///
    /// `false` when the tree is at its bounds and the file was not seeded. Mount-blind, for
    /// the reason [`Self::put_file`] states.
    pub fn seal_file(&mut self, path: &str, contents: &str) -> bool {
        let Some(resolved) = self.resolve(path) else {
            return false;
        };
        self.make_directories_of(&resolved);
        if !self.write_resolved(&resolved, contents, false) {
            return false;
        }
        self.sealed.insert(resolved);
        true
    }

    /// Add a device that KEEPS what is written to it, so the caller reads those lines back
    /// ([`Self::device_lines`]).
    pub fn add_keeping_device(&mut self, path: &str) {
        if let Some(resolved) = self.resolve(path) {
            self.make_directories_of(&resolved);
            self.devices.insert(resolved.clone(), Device::Keep(Vec::new()));
            self.record_written(&resolved);
        }
    }

    /// Add a device that DISCARDS what is written to it, which is the whole of what the
    /// null device does. Reading one answers nothing, as reading it does on a machine.
    pub fn add_discarding_device(&mut self, path: &str) {
        if let Some(resolved) = self.resolve(path) {
            self.make_directories_of(&resolved);
            self.devices.insert(resolved.clone(), Device::Discard);
            self.record_written(&resolved);
        }
    }

    /// Make `path` a directory, with every directory above it — `mkdir -p`'s whole effect.
    ///
    /// `false` when the tree is at [`VolumeLimits::directories`], or when a FILE already
    /// holds one of those paths, which is what makes the command fail rather than replace
    /// it.
    pub fn make_directories(&mut self, path: &str) -> bool {
        let Some(resolved) = self.resolve(path) else {
            return false;
        };
        if let Some(made) = self.in_mount_mut(&resolved, |files, inside| {
            files.make_directories(inside)
        }) {
            return made;
        }
        let mut built = String::new();
        let mut made: Vec<String> = Vec::new();
        for segment in resolved.split('/').filter(|segment| !segment.is_empty()) {
            built.push('/');
            built.push_str(segment);
            if self.files.contains_key(&built) || self.devices.contains_key(&built) {
                return false;
            }
            if !self.directories.contains(&built) {
                made.push(built.clone());
            }
        }
        if self.directories.len().saturating_add(made.len()) > self.limits.directories {
            return false;
        }
        for directory in &made {
            self.record_written(directory);
        }
        self.directories.extend(made);
        true
    }

    /// Record `path` as written NOW, which is what [`EntryFacts::modified`] reports for it.
    fn record_written(&mut self, path: &str) {
        self.modified.insert(path.to_string(), SystemTime::now());
    }

    /// Record the directory `path` lies in as written, which is what creating or removing
    /// something under a directory does to that directory.
    fn record_parent_written(&mut self, path: &str) {
        if let Some(parent) = Self::parent_of(path) {
            if self.directories.contains(&parent) {
                self.record_written(&parent);
            }
        }
    }

    /// The directories above `path`, made as far as the bounds allow. Used where a seed the
    /// SERVICE performs implies them: a machine that has the file has the directories.
    fn make_directories_of(&mut self, path: &str) {
        if let Some(parent) = Self::parent_of(path) {
            let _ = self.make_directories(&parent);
        }
    }

    /// What reading `path` answers: a file's contents, a sealed file's contents, or the
    /// empty text a discarding device answers — and `None` for a path the tree does not
    /// hold, a directory, or a device whose lines are kept (a console is written, not read).
    ///
    /// A path inside a MOUNTED filesystem is read out of that filesystem, which is where its
    /// files are.
    #[must_use]
    pub fn read(&self, path: &str) -> Option<String> {
        let resolved = self.resolve(path)?;
        if let Some(read) = self.in_mount(&resolved, |files, inside| files.read(inside)) {
            return read;
        }
        if let Some(Device::Discard) = self.devices.get(&resolved) {
            return Some(String::new());
        }
        self.files.get(&resolved).cloned()
    }

    /// Whether the tree holds anything at `path` at all — a file, a directory or a device.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_some_and(|resolved| {
            if let Some(holds) = self.in_mount(&resolved, |files, inside| files.exists(inside)) {
                return holds;
            }
            self.files.contains_key(&resolved)
                || self.directories.contains(&resolved)
                || self.devices.contains_key(&resolved)
        })
    }

    /// Whether `path` is a file of the tree (sealed or not).
    #[must_use]
    pub fn is_file(&self, path: &str) -> bool {
        self.resolve(path).is_some_and(|resolved| {
            if let Some(is_file) = self.in_mount(&resolved, |files, inside| files.is_file(inside)) {
                return is_file;
            }
            self.files.contains_key(&resolved)
        })
    }

    /// Whether `path` is a directory of the tree.
    #[must_use]
    pub fn is_directory(&self, path: &str) -> bool {
        self.resolve(path).is_some_and(|resolved| {
            if let Some(is_directory) =
                self.in_mount(&resolved, |files, inside| files.is_directory(inside))
            {
                return is_directory;
            }
            self.directories.contains(&resolved)
        })
    }

    /// Write `contents` at `path`, replacing what is there or APPENDING to it.
    ///
    /// `false` is a write that did not happen, which is what the command performing it
    /// reports as its own failure: a path this tree does not accept, a sealed file, a
    /// directory, a path whose directory the tree does not have, or a write past
    /// [`VolumeLimits::files`] or [`VolumeLimits::bytes`]. A device takes the write as the
    /// lines it was given, up to [`VolumeLimits::device_lines`].
    pub fn write(&mut self, path: &str, contents: &str, append: bool) -> bool {
        let Some(resolved) = self.resolve(path) else {
            return false;
        };
        // A path inside a MOUNTED filesystem is written to that filesystem, under its own
        // bounds, which is what makes the write one another client of it reads.
        if let Some(wrote) =
            self.in_mount_mut(&resolved, |files, inside| files.write(inside, contents, append))
        {
            return wrote;
        }
        if self.sealed.contains(&resolved) || self.directories.contains(&resolved) {
            return false;
        }
        if self.devices.contains_key(&resolved) {
            return self.write_device(&resolved, contents);
        }
        // A machine will not create a file in a directory it does not have.
        if Self::parent_of(&resolved).is_some_and(|parent| !self.directories.contains(&parent)) {
            return false;
        }
        self.write_resolved(&resolved, contents, append)
    }

    /// [`Self::write`] once the path is resolved and known to name a file.
    fn write_resolved(&mut self, resolved: &str, contents: &str, append: bool) -> bool {
        let held = self.files.get(resolved);
        if held.is_none() && self.files.len() >= self.limits.files {
            return false;
        }
        let kept = held.map_or(0, String::len);
        let addition = if append {
            contents.len()
        } else {
            contents.len().saturating_sub(kept)
        };
        let path_bytes = if held.is_none() { resolved.len() } else { 0 };
        let would_hold = if append {
            self.held_bytes.saturating_add(addition)
        } else {
            self.held_bytes
                .saturating_sub(kept)
                .saturating_add(contents.len())
        }
        .saturating_add(path_bytes);
        if would_hold > self.limits.bytes {
            return false;
        }
        self.held_bytes = would_hold;
        let created = held.is_none();
        match self.files.get_mut(resolved) {
            Some(file) if append => file.push_str(contents),
            Some(file) => *file = contents.to_string(),
            None => {
                self.files.insert(resolved.to_string(), contents.to_string());
            }
        }
        self.record_written(resolved);
        if created {
            self.record_parent_written(resolved);
        }
        true
    }

    /// Write to a DEVICE: one line per line of `contents`, kept or discarded as the device
    /// says. A line past [`VolumeLimits::device_line_bytes`] is truncated on a character
    /// boundary; a write past [`VolumeLimits::device_lines`] does not happen.
    fn write_device(&mut self, resolved: &str, contents: &str) -> bool {
        let limits = self.limits;
        let Some(device) = self.devices.get_mut(resolved) else {
            return false;
        };
        let Device::Keep(lines) = device else {
            return true;
        };
        // The device takes whole lines: a write with no newline of its own is still one
        // line, which is what a console shows for it.
        let written: Vec<&str> = contents.split_inclusive('\n').collect();
        if lines.len().saturating_add(written.len()) > limits.device_lines {
            return false;
        }
        for line in written {
            let mut line = line.trim_end_matches('\n').to_string();
            if line.len() > limits.device_line_bytes {
                let mut cut = limits.device_line_bytes;
                while cut > 0 && !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                line.truncate(cut);
            }
            lines.push(line);
        }
        self.record_written(resolved);
        true
    }

    /// Remove the file at `path`, answering whether there was one to remove. A sealed file,
    /// a directory and a device are not removed: a program does not take away what the
    /// machine published to it.
    pub fn remove_file(&mut self, path: &str) -> bool {
        let Some(resolved) = self.resolve(path) else {
            return false;
        };
        if let Some(removed) = self.in_mount_mut(&resolved, |files, inside| {
            files.remove_file(inside)
        }) {
            return removed;
        }
        if self.sealed.contains(&resolved) {
            return false;
        }
        match self.files.remove(&resolved) {
            Some(contents) => {
                self.held_bytes = self
                    .held_bytes
                    .saturating_sub(contents.len().saturating_add(resolved.len()));
                self.modified.remove(&resolved);
                self.record_parent_written(&resolved);
                true
            }
            None => false,
        }
    }

    /// What the tree holds at `path`, as a long listing of it reports that entry — and
    /// `None` for a path the tree holds nothing at.
    ///
    /// A FILE's size is the bytes it holds. A DIRECTORY holds the names of the entries under
    /// it, so its size is the bytes those names come to. A DEVICE keeps no bytes of its own,
    /// so its size is none.
    #[must_use]
    pub fn facts(&self, path: &str) -> Option<EntryFacts> {
        let resolved = self.resolve(path)?;
        if let Some(facts) = self.in_mount(&resolved, |files, inside| files.facts(inside)) {
            return facts;
        }
        let (kind, size) = if let Some(contents) = self.files.get(&resolved) {
            (EntryKind::File, contents.len())
        } else if self.directories.contains(&resolved) {
            let held = self
                .entry_names(&resolved)
                .iter()
                .map(|name| name.len())
                .sum();
            (EntryKind::Directory, held)
        } else if self.devices.contains_key(&resolved) {
            (EntryKind::Device, 0)
        } else {
            return None;
        };
        Some(EntryFacts {
            kind,
            size,
            // An entry the tree holds was written when it was put there; a tree whose entry
            // predates this record reports the start of the clock's own epoch rather than an
            // instant nothing happened at.
            modified: self
                .modified
                .get(&resolved)
                .copied()
                .unwrap_or(SystemTime::UNIX_EPOCH),
        })
    }

    /// The names of the entries directly under the DIRECTORY `path`, sorted — what a listing
    /// of a directory names. Empty for a path that is not a directory of this tree.
    #[must_use]
    pub fn entry_names(&self, path: &str) -> Vec<String> {
        let Some(resolved) = self.resolve(path) else {
            return Vec::new();
        };
        if let Some(names) = self.in_mount(&resolved, |files, inside| files.entry_names(inside)) {
            return names;
        }
        if !self.directories.contains(&resolved) {
            return Vec::new();
        }
        let under = if resolved == ROOT {
            ROOT.to_string()
        } else {
            format!("{resolved}/")
        };
        let mut names: Vec<String> = Vec::new();
        let held = self
            .files
            .keys()
            .chain(self.directories.iter())
            .chain(self.devices.keys());
        for entry in held {
            let Some(relative) = entry.strip_prefix(under.as_str()) else {
                continue;
            };
            // Only what lies DIRECTLY under the directory is an entry of it: a deeper path
            // names the directory between them instead.
            let name = relative.split('/').next().unwrap_or(relative);
            if !name.is_empty() && !names.iter().any(|held| held == name) {
                names.push(name.to_string());
            }
        }
        names.sort();
        names
    }

    /// The lines written to the device at `path`, in the order they were written — empty for
    /// a path that is not a keeping device.
    #[must_use]
    pub fn device_lines(&self, path: &str) -> &[String] {
        match self.resolve(path).and_then(|resolved| {
            self.devices.get(&resolved).map(|device| match device {
                Device::Keep(lines) => lines.as_slice(),
                Device::Discard => &[],
            })
        }) {
            Some(lines) => lines,
            None => &[],
        }
    }

    /// The files a program may write, as `(path, contents)` — what the caller keeps beside
    /// the machine so the next run of a program on it reads what this one wrote. The sealed
    /// files are left out: they are the service's own seed, not the machine's storage.
    pub fn stored_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files
            .iter()
            .filter(|(path, _)| !self.sealed.contains(*path))
            .map(|(path, contents)| (path.as_str(), contents.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::{EntryKind, ShellVolume, VolumeLimits};

    /// RULE: a path is resolved the way a machine resolves one — absolute, with empty, `.`
    /// and `..` segments resolved and `..` above the root staying at the root — and a
    /// relative path is one this tree does not accept at all.
    #[test]
    fn a_path_is_resolved_the_way_a_machine_resolves_one() {
        let volume = ShellVolume::default();
        // (case, the path written, what it resolves to)
        let rows = [
            ("already resolved", "/var/lib/counter", Some("/var/lib/counter")),
            ("a doubled separator", "/var//lib///counter", Some("/var/lib/counter")),
            ("a here segment", "/var/./lib/counter", Some("/var/lib/counter")),
            ("an up segment", "/var/lib/../counter", Some("/var/counter")),
            ("up above the root", "/../../counter", Some("/counter")),
            ("the root itself", "/", Some("/")),
            ("only up segments", "/..", Some("/")),
            ("a relative path", "var/lib/counter", None),
            ("an empty path", "", None),
        ];
        for (case, path, expected) in rows {
            let resolved = volume.resolve(path);
            assert_eq!(
                resolved.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }

    /// RULE: what a program may do to an entry is what the entry IS — a file it may read and
    /// write, a sealed file it may only read, a keeping device it may only write, a
    /// discarding device that answers nothing — so no write replaces what the service
    /// published to the guest.
    #[test]
    fn what_a_program_may_do_to_an_entry_is_what_the_entry_is() {
        let mut volume = ShellVolume::default();
        volume.put_file("/var/lib/counter", "1");
        volume.seal_file("/proc/sys/kernel/random/boot_id", "a-boot-id");
        volume.add_keeping_device("/dev/console");
        volume.add_discarding_device("/dev/null");

        // (case, path, may be read to, may be written)
        let rows = [
            ("a stored file", "/var/lib/counter", Some("1"), true),
            (
                "a sealed file",
                "/proc/sys/kernel/random/boot_id",
                Some("a-boot-id"),
                false,
            ),
            ("a keeping device", "/dev/console", None, true),
            ("a discarding device", "/dev/null", Some(""), true),
            ("a path the tree does not hold yet", "/var/lib/fresh", None, true),
            ("a directory", "/var/lib", None, false),
        ];
        for (case, path, readable, writable) in rows {
            let read = volume.read(path);
            assert_eq!(
                read.as_deref(),
                readable,
                "case {case:?}: expected a read of {readable:?}, got {read:?}"
            );
            let wrote = volume.write(path, "written\n", true);
            assert_eq!(
                wrote, writable,
                "case {case:?}: expected a write to answer {writable}, got {wrote}"
            );
        }
        assert_eq!(
            volume.read("/proc/sys/kernel/random/boot_id").as_deref(),
            Some("a-boot-id"),
            "the refused write left the sealed file as the service seeded it"
        );
        assert_eq!(
            volume.device_lines("/dev/console"),
            ["written"],
            "the device kept the line written to it, got {:?}",
            volume.device_lines("/dev/console")
        );
    }

    /// RULE: a write to a path whose directory the machine does not have fails, and `mkdir
    /// -p` is what makes it succeed.
    #[test]
    fn a_write_needs_the_directory_above_it() {
        let mut volume = ShellVolume::default();
        assert!(
            !volume.write("/var/lib/sim/counter", "1", false),
            "a write into a directory the tree does not have did not fail"
        );
        assert!(
            volume.make_directories("/var/lib/sim"),
            "mkdir -p did not make the directories"
        );
        assert!(
            volume.write("/var/lib/sim/counter", "1", false),
            "a write into the directory just made did not succeed"
        );
        assert_eq!(volume.read("/var/lib/sim/counter").as_deref(), Some("1"));
    }

    /// RULE: every bound is the volume's own, and a write past one does not happen.
    #[test]
    fn every_bound_refuses_the_write() {
        let limits = VolumeLimits {
            files: 2,
            bytes: 64,
            path_bytes: 32,
            directories: 4,
            device_lines: 2,
            device_line_bytes: 8,
        };
        let mut volume = ShellVolume::new(limits);
        assert!(volume.write("/one", "x", false), "the first file fits");
        assert!(volume.write("/two", "x", false), "the second file fits");
        assert!(
            !volume.write("/three", "x", false),
            "a file past the file count was still written"
        );
        assert!(
            !volume.write("/one", &"x".repeat(128), false),
            "a write past the byte bound was still performed"
        );
        assert!(
            !volume.write(&format!("/{}", "p".repeat(64)), "x", false),
            "a path past the path bound was still accepted"
        );
        volume.add_keeping_device("/dev/console");
        assert!(
            volume.write("/dev/console", "first\nsecond\n", true),
            "the device took the lines that fit"
        );
        assert!(
            !volume.write("/dev/console", "third\n", true),
            "a device write past the line bound was still kept"
        );
        assert_eq!(
            volume.device_lines("/dev/console").len(),
            2,
            "the device kept more lines than its bound allows"
        );
        let mut truncating = ShellVolume::new(limits);
        truncating.add_keeping_device("/dev/console");
        truncating.write("/dev/console", "0123456789abcdef\n", true);
        assert_eq!(
            truncating.device_lines("/dev/console"),
            ["01234567"],
            "a line past the device's line bound was not truncated, got {:?}",
            truncating.device_lines("/dev/console")
        );
    }

    /// RULE: the files a program wrote are what the caller takes back out, and the service's
    /// own sealed seed is not among them.
    #[test]
    fn the_files_a_program_wrote_come_back_out() {
        let mut volume = ShellVolume::default();
        volume.seal_file("/proc/sys/kernel/random/boot_id", "a-boot-id");
        volume.put_file("/var/lib/counter", "1");
        volume.write("/var/lib/counter", "2", false);
        volume.make_directories("/var/log");
        volume.write("/var/log/boot", "up\n", true);
        let stored: Vec<(&str, &str)> = volume.stored_files().collect();
        assert_eq!(
            stored,
            [("/var/lib/counter", "2"), ("/var/log/boot", "up\n")],
            "expected only the files the program may write, got {stored:?}"
        );
        assert!(
            volume.remove_file("/var/log/boot"),
            "a file the program wrote could not be removed"
        );
        assert!(
            !volume.remove_file("/proc/sys/kernel/random/boot_id"),
            "a sealed file was removed by the program"
        );
    }

    /// RULE: what a listing of the tree reports about an entry is the entry's own facts —
    /// what it is, the bytes it holds and when it was last written — and a listing of a
    /// directory names the entries directly under it, each once.
    #[test]
    fn a_listing_reports_each_entrys_own_facts() {
        let mut volume = ShellVolume::default();
        let before = ::std::time::SystemTime::now();
        volume.make_directories("/data/deep/under");
        volume.write("/data/payload", "abcdef\n", false);
        volume.add_discarding_device("/data/sink");

        // (case, path, what it is, the bytes it holds)
        let rows = [
            ("a file", "/data/payload", Some((EntryKind::File, 7))),
            ("a directory", "/data/deep", Some((EntryKind::Directory, "under".len()))),
            ("a device", "/data/sink", Some((EntryKind::Device, 0))),
            ("a path the tree holds nothing at", "/data/absent", None),
            ("a relative path", "data/payload", None),
        ];
        for (case, path, expected) in rows {
            let facts = volume.facts(path);
            let reported = facts.map(|facts| (facts.kind, facts.size));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
            if let Some(facts) = facts {
                assert!(
                    facts.modified >= before,
                    "case {case:?}: expected a write instant at or after {before:?}, got {:?}",
                    facts.modified
                );
            }
        }

        let names = volume.entry_names("/data");
        assert_eq!(
            names,
            ["deep", "payload", "sink"],
            "expected the entries directly under the directory, got {names:?}"
        );
        let root = volume.entry_names("/");
        assert_eq!(
            root, ["data"],
            "expected the root to hold the one directory made under it, got {root:?}"
        );
        let of_a_file = volume.entry_names("/data/payload");
        assert!(
            of_a_file.is_empty(),
            "a path that is not a directory names no entries, got {of_a_file:?}"
        );
    }
}
