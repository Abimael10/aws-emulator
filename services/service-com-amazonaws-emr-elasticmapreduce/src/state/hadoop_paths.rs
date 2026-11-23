// SPDX-License-Identifier: CC0-1.0

//! What a HADOOP PATH names, and what the filesystem it names does with it.
//!
//! # The rule this module implements
//!
//! A Hadoop path names its filesystem by SCHEME — "Amazon EMR and Hadoop provide a variety of
//! file systems that you can use when processing cluster steps … HDFS and EMRFS are the two
//! main file systems used with Amazon EMR" (Amazon EMR Management Guide, "Work with storage and
//! file systems"), beside the node's own local filesystem under `file://` — and a path written
//! with NO scheme names the filesystem `fs.defaultFS` resolves to, which on an Amazon EMR
//! cluster is the cluster's HDFS. So one reading of a path decides which of three filesystems
//! an operation lands in:
//!
//! * `hdfs://`, and every unqualified path, is the cluster's own HDFS, held beside the cluster
//!   ([`super::node_file_systems`]) — a relative path under the home directory of the user the
//!   cluster's commands run as, as Hadoop resolves one;
//! * `s3://`, `s3a://` and `s3n://` are objects of Amazon S3, read and written through the same
//!   program a step's command line makes its other Amazon S3 calls with
//!   ([`super::step_command_transfers`]), so one command's reads and writes share its
//!   credential and its budget and the object a command wrote is a real object of this
//!   emulator's store — and a DIRECTORY of that filesystem is the zero-length marker object a
//!   filesystem over an object store writes for one ([`DIRECTORY_MARKER`]);
//! * `file://` is the NODE's own local filesystem, which is the tree the session's other
//!   commands read and write ([`::service_common::posix_shell_session::ProgramFiles`]) and what
//!   the node keeps between programs.
//!
//! Why a refusal is worded the way it is: a filesystem answers an operation with the kernel's
//! own error, and Hadoop's filesystem shell reports that error against the path it was given
//! ([`HadoopRefusal::reported`]), so the error TEXT is the shared one this workspace states once
//! ([`::service_common::posix_shell_files::FileRefusal`]) rather than a second wording of the
//! same errors.

use ::std::time::SystemTime;

use ::service_common::posix_shell_files::FileRefusal;
use ::service_common::posix_shell_session::ProgramFiles;
use ::service_common::posix_shell_volume::{resolved_path, EntryKind};

use super::*;

/// How many objects ONE listing of the store walks. CHOSEN: a bound on what one command's
/// listing costs, far above the entries a step's command line walks.
const MAX_LISTED_OBJECTS: usize = 256;

/// How many entries ONE disk-usage walk of a filesystem's own tree visits, and how deep it
/// goes. CHOSEN for the same reason: a bound on what one command costs, above any tree a step's
/// command line walks. A walk past either bound answers that bound rather than a partial sum.
const MAX_WALKED_ENTRIES: usize = 256;
const MAX_WALK_DEPTH: usize = 32;

/// The CONTENTS of the object a filesystem over the object store writes for a DIRECTORY: a
/// zero-length object whose key ends in the separator is what makes the place that key names
/// exist before anything is put under it. Amazon EMR's own filesystem writes one for `-mkdir`,
/// and the listings here skip such a key the way a filesystem over the store does.
const DIRECTORY_MARKER: &str = "";

/// What a Hadoop path names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HadoopPath {
    /// A path of the cluster's own HDFS, resolved absolute.
    Hdfs(String),
    /// An object of Amazon S3, or the PLACE a key with a trailing separator (or no key at all)
    /// names, read and written through EMRFS.
    Object { bucket: String, key: String },
    /// A path of the NODE's own local filesystem.
    Local(String),
    /// The command's own standard input or standard output, which the filesystem shell takes as
    /// the operand `-`.
    Stream,
}

/// The operand Hadoop's filesystem shell reads the command's own standard input from, and
/// writes its standard output to.
pub const STREAM_PATH: &str = "-";

/// What the path `written` names, and `None` when it names nothing this cluster has a
/// filesystem for.
///
/// `local` is whether a path with no scheme is the NODE's: it is for the operand a command
/// takes as a local one by definition (`-put`'s source, `-get`'s destination), and for every
/// other operand an unqualified path is a path of the default filesystem, which is HDFS.
#[must_use]
pub fn hadoop_path(written: &str, local: bool) -> Option<HadoopPath> {
    if written == STREAM_PATH {
        return Some(HadoopPath::Stream);
    }
    if written.is_empty() {
        return None;
    }
    if let Some(object) = addressed_object(written) {
        return Some(object);
    }
    if let Some(path) = written.strip_prefix(LOCAL_FILE_SCHEME) {
        // `file:///tmp/x` names the local path after the (empty) authority; a `file://` URI
        // with anything else as its authority names a host this node is not.
        return path.starts_with('/').then(|| HadoopPath::Local(path.to_string()));
    }
    if let Some(rest) = written.strip_prefix(HDFS_SCHEME) {
        // `hdfs:///a/b` and `hdfs://<name-node>/a/b` both name the cluster's own HDFS: the
        // authority is the NameNode, and this cluster has one.
        let separator = rest.find('/')?;
        return resolved_path(&rest[separator..]).map(HadoopPath::Hdfs);
    }
    if written.contains("://") {
        // A scheme naming a filesystem this cluster does not have.
        return None;
    }
    if local {
        return Some(HadoopPath::Local(written.to_string()));
    }
    // An unqualified path is a path of the default filesystem, resolved against the home
    // directory of the user the cluster's commands run as when it is relative.
    let absolute = if written.starts_with('/') {
        written.to_string()
    } else {
        format!("{HDFS_HOME_DIRECTORY}/{written}")
    };
    resolved_path(&absolute).map(HadoopPath::Hdfs)
}

/// The object or place `written` addresses, and `None` when it is no Amazon S3 address at all.
fn addressed_object(written: &str) -> Option<HadoopPath> {
    let rest = S3_URI_SCHEMES
        .iter()
        .find_map(|scheme| written.strip_prefix(scheme))?;
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    (!bucket.is_empty()).then(|| HadoopPath::Object {
        bucket: bucket.to_string(),
        key: key.to_string(),
    })
}

impl HadoopPath {
    /// This path as the command line wrote it, which is how a diagnostic about it names it.
    #[must_use]
    pub fn shown(&self) -> String {
        match self {
            Self::Hdfs(path) => path.clone(),
            Self::Object { bucket, key } => format!("s3://{bucket}/{key}"),
            Self::Local(path) => path.clone(),
            Self::Stream => STREAM_PATH.to_string(),
        }
    }

    /// The last segment of this path, which is the name a file carried INTO a directory keeps.
    #[must_use]
    pub fn base_name(&self) -> String {
        match self {
            Self::Hdfs(path) | Self::Local(path) => {
                path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
            }
            Self::Object { key, .. } => key
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(key)
                .to_string(),
            Self::Stream => String::new(),
        }
    }

    /// This path with `name` under it, which is where a file carried into a DIRECTORY lands.
    #[must_use]
    pub fn joined(&self, name: &str) -> Self {
        match self {
            Self::Hdfs(path) => Self::Hdfs(format!("{}/{name}", path.trim_end_matches('/'))),
            Self::Local(path) => Self::Local(format!("{}/{name}", path.trim_end_matches('/'))),
            Self::Object { bucket, key } => Self::Object {
                bucket: bucket.clone(),
                key: match key.trim_end_matches('/') {
                    "" => name.to_string(),
                    trimmed => format!("{trimmed}/{name}"),
                },
            },
            Self::Stream => Self::Stream,
        }
    }
}

/// What one entry of a filesystem IS, as a listing of it reports that entry.
#[derive(Debug, Clone)]
pub struct HadoopEntry {
    pub kind: EntryKind,
    /// The bytes it holds.
    pub size: u64,
    /// When it was last written, and `None` for a filesystem that keeps none for it.
    pub modified: Option<SystemTime>,
}

/// Why a filesystem operation did not happen.
#[derive(Debug, Clone)]
pub enum HadoopRefusal {
    /// The filesystem's own error about the path.
    Path(FileRefusal),
    /// What the store, or a bound of this emulator, answered instead — which is the detail the
    /// command reports.
    Detail(String),
}

impl HadoopRefusal {
    /// This refusal as the filesystem shell reports it for the command `command` on `path`:
    /// the shell writes the command, the path it was given and the error, in that order.
    #[must_use]
    pub fn reported(&self, command: &str, path: &str) -> String {
        match self {
            Self::Path(refusal) => format!("{command}: `{path}': {}", refusal.message()),
            Self::Detail(detail) => format!("{command}: `{path}': {detail}"),
        }
    }
}

/// The three filesystems the paths of a cluster's command lines name.
#[derive(Debug, Clone)]
pub struct HadoopFileSystems {
    /// The cluster's own HDFS.
    pub(super) hdfs: ClusterHdfs,
    /// The program every Amazon S3 call of this command is made through, and `None` where the
    /// command reaches no object store at all.
    pub(super) objects: Option<StepObjectCli>,
    /// What Hadoop resolves on the cluster's nodes, which is what a listing reports for the
    /// replication and the block size of a file.
    pub(super) configuration: HadoopConfiguration,
}

impl HadoopFileSystems {
    /// The object program, or the refusal a command that reaches no object store answers.
    fn objects(&self) -> Result<&StepObjectCli, HadoopRefusal> {
        self.objects.as_ref().ok_or_else(|| {
            HadoopRefusal::Detail("No Amazon S3 client was available to this command.".to_string())
        })
    }

    /// What reading `at` answers.
    pub fn read(
        &self,
        at: &HadoopPath,
        standard_input: Option<&str>,
        files: &dyn ProgramFiles,
    ) -> Result<String, HadoopRefusal> {
        match at {
            HadoopPath::Hdfs(path) => {
                if self.hdfs.is_directory(path) {
                    return Err(HadoopRefusal::Path(FileRefusal::IsADirectory));
                }
                self.hdfs
                    .read(path)
                    .ok_or(HadoopRefusal::Path(FileRefusal::NotFound))
            }
            HadoopPath::Object { bucket, key } => {
                if key.is_empty() || key.ends_with('/') {
                    return Err(HadoopRefusal::Path(FileRefusal::IsADirectory));
                }
                let address = S3Object {
                    bucket: bucket.clone(),
                    key: key.clone(),
                };
                self.objects()?
                    .get_object(&address)
                    .map_err(HadoopRefusal::Detail)
            }
            HadoopPath::Local(path) => files
                .read(path)
                .ok_or(HadoopRefusal::Path(FileRefusal::NotFound)),
            HadoopPath::Stream => standard_input
                .map(str::to_string)
                .ok_or(HadoopRefusal::Path(FileRefusal::NotFound)),
        }
    }

    /// Write `contents` at `at`, answering the lines the command WROTE where the destination is
    /// the command's own standard output.
    pub fn write(
        &self,
        at: &HadoopPath,
        contents: &str,
        files: &mut dyn ProgramFiles,
    ) -> Result<Vec<String>, HadoopRefusal> {
        match at {
            HadoopPath::Hdfs(path) => {
                if self.hdfs.is_directory(path) {
                    return Err(HadoopRefusal::Path(FileRefusal::IsADirectory));
                }
                if self.hdfs.write(path, contents) {
                    return Ok(Vec::new());
                }
                Err(HadoopRefusal::Path(FileRefusal::NoSpace))
            }
            HadoopPath::Object { bucket, key } => {
                if key.is_empty() || key.ends_with('/') {
                    return Err(HadoopRefusal::Path(FileRefusal::IsADirectory));
                }
                let address = S3Object {
                    bucket: bucket.clone(),
                    key: key.clone(),
                };
                self.objects()?
                    .put_object(&address, contents)
                    .map(|()| Vec::new())
                    .map_err(HadoopRefusal::Detail)
            }
            HadoopPath::Local(path) => {
                // A filesystem's own create makes the directories above the file it creates.
                if let Some(parent) = ::service_common::posix_shell_volume::parent_path(path) {
                    files.make_directories(&parent);
                }
                if files.write(path, contents) {
                    return Ok(Vec::new());
                }
                Err(HadoopRefusal::Path(FileRefusal::NotFound))
            }
            // The bytes reach the stream the stage after this command reads, and nothing else.
            HadoopPath::Stream => Ok(contents.lines().map(str::to_string).collect()),
        }
    }

    /// Make `at` a directory, with every directory above it.
    ///
    /// Amazon S3 holds objects rather than directories, so a filesystem over it marks the place
    /// a directory names with a zero-length object whose key ends in the separator
    /// ([`DIRECTORY_MARKER`]): that is what makes the place exist for a later listing or test,
    /// and it is skipped by every listing here the way a filesystem over the store skips it.
    pub fn make_directories(
        &self,
        at: &HadoopPath,
        files: &mut dyn ProgramFiles,
    ) -> Result<(), HadoopRefusal> {
        match at {
            HadoopPath::Hdfs(path) => self
                .hdfs
                .make_directories(path)
                .then_some(())
                .ok_or(HadoopRefusal::Path(FileRefusal::NotADirectory)),
            HadoopPath::Object { bucket, key } => {
                let prefix = place_prefix(key);
                if prefix.is_empty() {
                    // The BUCKET itself, which the object store already holds or does not.
                    return Ok(());
                }
                self.objects()?
                    .put_object(
                        &S3Object {
                            bucket: bucket.clone(),
                            key: prefix,
                        },
                        DIRECTORY_MARKER,
                    )
                    .map_err(HadoopRefusal::Detail)
            }
            HadoopPath::Local(path) => files
                .make_directories(path)
                .then_some(())
                .ok_or(HadoopRefusal::Path(FileRefusal::NotADirectory)),
            HadoopPath::Stream => Err(HadoopRefusal::Path(FileRefusal::NotFound)),
        }
    }

    /// Remove `at`, and everything under it when `recursive`.
    pub fn remove(
        &self,
        at: &HadoopPath,
        recursive: bool,
        files: &mut dyn ProgramFiles,
    ) -> Result<(), HadoopRefusal> {
        match at {
            HadoopPath::Hdfs(path) => {
                if self.hdfs.is_directory(path) && !recursive {
                    return Err(HadoopRefusal::Path(FileRefusal::IsADirectory));
                }
                let removed = if recursive {
                    self.hdfs.remove_tree(path)
                } else {
                    self.hdfs.remove_file(path)
                };
                removed
                    .then_some(())
                    .ok_or(HadoopRefusal::Path(FileRefusal::NotFound))
            }
            HadoopPath::Object { bucket, key } => {
                let objects = self.objects()?;
                // A key naming a PLACE, and a recursive removal, take away every object under
                // it: that is what the place was.
                let names_a_place = key.is_empty() || key.ends_with('/');
                if recursive || names_a_place {
                    let prefix = place_prefix(key);
                    let listed = objects
                        .listed_objects(bucket, &prefix, MAX_LISTED_OBJECTS)
                        .map_err(HadoopRefusal::Detail)?;
                    if listed.is_empty() && names_a_place {
                        return Err(HadoopRefusal::Path(FileRefusal::NotFound));
                    }
                    for object in &listed {
                        objects
                            .delete_object(&S3Object {
                                bucket: bucket.clone(),
                                key: object.key.clone(),
                            })
                            .map_err(HadoopRefusal::Detail)?;
                    }
                    if !names_a_place {
                        // The key itself is an object as well where the store holds one.
                        let address = S3Object {
                            bucket: bucket.clone(),
                            key: key.clone(),
                        };
                        objects
                            .delete_object(&address)
                            .map_err(HadoopRefusal::Detail)?;
                    }
                    return Ok(());
                }
                let address = S3Object {
                    bucket: bucket.clone(),
                    key: key.clone(),
                };
                // A removal answers for an object the store HOLDS, so one it holds nothing at
                // is the shell's own `No such file or directory` rather than a silent success.
                if objects
                    .object_facts(&address)
                    .map_err(HadoopRefusal::Detail)?
                    .is_none()
                {
                    return Err(HadoopRefusal::Path(FileRefusal::NotFound));
                }
                objects
                    .delete_object(&address)
                    .map_err(HadoopRefusal::Detail)
            }
            HadoopPath::Local(path) => files
                .remove(path)
                .then_some(())
                .ok_or(HadoopRefusal::Path(FileRefusal::NotFound)),
            HadoopPath::Stream => Err(HadoopRefusal::Path(FileRefusal::NotFound)),
        }
    }

    /// What the filesystem holds at `at`, and `None` when it holds nothing there.
    pub fn entry(
        &self,
        at: &HadoopPath,
        files: &dyn ProgramFiles,
    ) -> Result<Option<HadoopEntry>, HadoopRefusal> {
        match at {
            HadoopPath::Hdfs(path) => Ok(self.hdfs.facts(path).map(|facts| HadoopEntry {
                kind: facts.kind,
                size: u64::try_from(facts.size).unwrap_or_default(),
                modified: Some(facts.modified),
            })),
            HadoopPath::Object { bucket, key } => {
                let objects = self.objects()?;
                if !key.is_empty() && !key.ends_with('/') {
                    if let Some((size, modified)) = objects
                        .object_facts(&S3Object {
                            bucket: bucket.clone(),
                            key: key.clone(),
                        })
                        .map_err(HadoopRefusal::Detail)?
                    {
                        return Ok(Some(HadoopEntry {
                            kind: EntryKind::File,
                            size,
                            modified,
                        }));
                    }
                }
                // A place exists as long as something lies under it.
                let prefix = place_prefix(key);
                let listed = objects
                    .listed_objects(bucket, &prefix, 1)
                    .map_err(HadoopRefusal::Detail)?;
                Ok((!listed.is_empty()).then_some(HadoopEntry {
                    kind: EntryKind::Directory,
                    size: 0,
                    modified: None,
                }))
            }
            HadoopPath::Local(path) => {
                if let Some(contents) = files.read(path) {
                    return Ok(Some(HadoopEntry {
                        kind: EntryKind::File,
                        size: u64::try_from(contents.len()).unwrap_or_default(),
                        // The node's local disk keeps the file, not an instant for it.
                        modified: None,
                    }));
                }
                Ok((!files.entries_in(path).is_empty()).then_some(HadoopEntry {
                    kind: EntryKind::Directory,
                    size: 0,
                    modified: None,
                }))
            }
            HadoopPath::Stream => Ok(None),
        }
    }

    /// Whether `at` is a directory of its filesystem.
    pub fn is_directory(&self, at: &HadoopPath, files: &dyn ProgramFiles) -> bool {
        self.entry(at, files)
            .ok()
            .flatten()
            .is_some_and(|entry| entry.kind == EntryKind::Directory)
    }

    /// The bytes at or under `at` come to — what a disk-usage walk of it answers.
    ///
    /// A directory is walked WHOLE, as the command's own walk is: the prefix listing for a place
    /// of the object store, the filesystem's own files for a directory of HDFS, and the tree
    /// itself for the node's files, bounded by [`MAX_WALKED_ENTRIES`] and [`MAX_WALK_DEPTH`].
    pub fn size_under(
        &self,
        at: &HadoopPath,
        files: &dyn ProgramFiles,
    ) -> Result<u64, HadoopRefusal> {
        let Some(entry) = self.entry(at, files)? else {
            return Err(HadoopRefusal::Path(FileRefusal::NotFound));
        };
        if entry.kind == EntryKind::File {
            return Ok(entry.size);
        }
        match at {
            HadoopPath::Object { bucket, key } => {
                let prefix = place_prefix(key);
                Ok(self
                    .objects()?
                    .listed_objects(bucket, &prefix, MAX_LISTED_OBJECTS)
                    .map_err(HadoopRefusal::Detail)?
                    .iter()
                    .map(|object| object.size)
                    .sum())
            }
            HadoopPath::Hdfs(path) => Ok(self.hdfs.bytes_under(path)),
            // The node's own tree keeps no walk of its own, so the directories under it are
            // walked here, bounded.
            _ => {
                let mut held = 0u64;
                let mut walked = 0usize;
                let mut pending: Vec<(HadoopPath, usize)> = vec![(at.clone(), 0)];
                while let Some((one, depth)) = pending.pop() {
                    walked = walked.saturating_add(1);
                    if walked > MAX_WALKED_ENTRIES || depth > MAX_WALK_DEPTH {
                        return Err(HadoopRefusal::Detail(format!(
                            "The walk is past the {MAX_WALKED_ENTRIES} entries and \
                             {MAX_WALK_DEPTH} levels this emulator walks for one command."
                        )));
                    }
                    for (under, entry) in self.listed(&one, files)? {
                        match entry.kind {
                            EntryKind::Directory => {
                                pending.push((under, depth.saturating_add(1)));
                            }
                            _ => held = held.saturating_add(entry.size),
                        }
                    }
                }
                Ok(held)
            }
        }
    }

    /// The entries directly under the directory `at`, as `(path, entry)` in path order.
    pub fn listed(
        &self,
        at: &HadoopPath,
        files: &dyn ProgramFiles,
    ) -> Result<Vec<(HadoopPath, HadoopEntry)>, HadoopRefusal> {
        match at {
            HadoopPath::Object { bucket, key } => {
                let prefix = place_prefix(key);
                let listed = self
                    .objects()?
                    .listed_objects(bucket, &prefix, MAX_LISTED_OBJECTS)
                    .map_err(HadoopRefusal::Detail)?;
                let mut entries: Vec<(HadoopPath, HadoopEntry)> = Vec::new();
                for object in listed {
                    let Some(relative) = object.key.strip_prefix(prefix.as_str()) else {
                        continue;
                    };
                    let (name, kind) = match relative.split_once('/') {
                        Some((directory, _)) => (directory.to_string(), EntryKind::Directory),
                        None => (relative.to_string(), EntryKind::File),
                    };
                    if name.is_empty() {
                        continue;
                    }
                    let under = HadoopPath::Object {
                        bucket: bucket.clone(),
                        key: format!("{prefix}{name}"),
                    };
                    if entries.iter().any(|(held, _)| *held == under) {
                        continue;
                    }
                    let (size, modified) = match kind {
                        EntryKind::File => (object.size, object.modified),
                        _ => (0, None),
                    };
                    entries.push((
                        under,
                        HadoopEntry {
                            kind,
                            size,
                            modified,
                        },
                    ));
                }
                Ok(entries)
            }
            HadoopPath::Hdfs(path) => {
                let mut entries: Vec<(HadoopPath, HadoopEntry)> = Vec::new();
                for name in self.hdfs.entry_names(path) {
                    let under = at.joined(&name);
                    if let Some(entry) = self.entry(&under, files)? {
                        entries.push((under, entry));
                    }
                }
                Ok(entries)
            }
            HadoopPath::Local(path) => {
                let mut entries: Vec<(HadoopPath, HadoopEntry)> = Vec::new();
                for name in files.entries_in(path) {
                    let under = at.joined(&name);
                    if let Some(entry) = self.entry(&under, files)? {
                        entries.push((under, entry));
                    }
                }
                Ok(entries)
            }
            HadoopPath::Stream => Ok(Vec::new()),
        }
    }

    /// How many copies of each block the filesystem `at` lies on keeps: the cluster's own
    /// replication for a path of HDFS, and one for a filesystem over an object store, which
    /// keeps the object itself.
    #[must_use]
    pub fn replication(&self, at: &HadoopPath) -> u32 {
        match at {
            HadoopPath::Hdfs(_) => self.configuration.replication(),
            _ => 1,
        }
    }

    /// The block size a file of this cluster is written in, as the cluster's resolved
    /// configuration states it.
    #[must_use]
    pub fn block_size(&self) -> u64 {
        self.configuration.block_size()
    }
}

/// The key PREFIX the place `key` names: the key with the one separator a place's keys lie
/// under, and nothing at all for a bucket.
fn place_prefix(key: &str) -> String {
    match key.trim_end_matches('/') {
        "" => String::new(),
        trimmed => format!("{trimmed}/"),
    }
}

#[cfg(test)]
mod tests {
    use super::{hadoop_path, HadoopPath};

    /// RULE: a Hadoop path names its filesystem by scheme, and a path with no scheme names the
    /// default filesystem — which is the cluster's HDFS, with a relative path under the home
    /// directory of the user the cluster's commands run as.
    ///
    /// The rows span every scheme a cluster has a filesystem for, the two forms of the HDFS
    /// authority, a relative and an absolute unqualified path, the operand that is the command's
    /// own stream, an operand a command takes as local by definition, and the schemes and forms
    /// a cluster has no filesystem for.
    #[test]
    fn a_path_names_the_filesystem_its_scheme_does() {
        let object = |bucket: &str, key: &str| HadoopPath::Object {
            bucket: bucket.to_string(),
            key: key.to_string(),
        };
        // (case, written, whether an unqualified path is local, what it names)
        let rows: [(&str, &str, bool, Option<HadoopPath>); 14] = [
            (
                "an HDFS path with no authority",
                "hdfs:///sim2real/one.txt",
                false,
                Some(HadoopPath::Hdfs("/sim2real/one.txt".to_string())),
            ),
            (
                "an HDFS path naming the name node",
                "hdfs://ip-10-0-0-1.ec2.internal:8020/sim2real/one.txt",
                false,
                Some(HadoopPath::Hdfs("/sim2real/one.txt".to_string())),
            ),
            (
                "an unqualified absolute path, which is the default filesystem",
                "/sim2real/one.txt",
                false,
                Some(HadoopPath::Hdfs("/sim2real/one.txt".to_string())),
            ),
            (
                "an unqualified relative path, under the cluster user's home directory",
                "out/one.txt",
                false,
                Some(HadoopPath::Hdfs("/user/hadoop/out/one.txt".to_string())),
            ),
            (
                "an unqualified path of an operand that is local by definition",
                "/tmp/one.txt",
                true,
                Some(HadoopPath::Local("/tmp/one.txt".to_string())),
            ),
            (
                "an Amazon S3 object",
                "s3://reports/out/one.txt",
                false,
                Some(object("reports", "out/one.txt")),
            ),
            (
                "the same object under the s3a scheme",
                "s3a://reports/out/one.txt",
                false,
                Some(object("reports", "out/one.txt")),
            ),
            (
                "the same object under the s3n scheme",
                "s3n://reports/out/one.txt",
                false,
                Some(object("reports", "out/one.txt")),
            ),
            (
                "a place under a bucket",
                "s3://reports/out/",
                false,
                Some(object("reports", "out/")),
            ),
            (
                "a bucket with nothing under it",
                "s3://reports",
                false,
                Some(object("reports", "")),
            ),
            (
                "a path of the node's own filesystem",
                "file:///tmp/one.txt",
                false,
                Some(HadoopPath::Local("/tmp/one.txt".to_string())),
            ),
            (
                "the command's own stream",
                "-",
                false,
                Some(HadoopPath::Stream),
            ),
            (
                "a scheme this cluster has no filesystem for",
                "gs://reports/out/one.txt",
                false,
                None,
            ),
            ("no path at all", "", false, None),
        ];
        for (case, written, local, expected) in rows {
            let named = hadoop_path(written, local);
            assert_eq!(
                named, expected,
                "case {case:?} ({written}): expected {expected:?}, got {named:?}"
            );
        }
    }
}
