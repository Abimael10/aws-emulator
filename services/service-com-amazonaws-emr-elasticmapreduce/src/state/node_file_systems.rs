// SPDX-License-Identifier: CC0-1.0

//! The FILESYSTEMS a cluster's nodes hold: the cluster's own HDFS, and each node's local disk.
//!
//! # The rule this module implements
//!
//! A cluster's bootstrap actions and its steps run ON ITS NODES — "a list of bootstrap actions
//! to run before Hadoop starts on the cluster nodes" (`RunJobFlow$BootstrapActions` doc), and
//! "command-runner.jar … lets you run commands on your cluster" (Amazon EMR documentation) —
//! and a node has STORAGE, which outlives the one program that wrote to it: what a bootstrap
//! action left on a node's local disk a later step of that node reads, and what one step wrote
//! into the cluster's HDFS the next step reads. So neither filesystem can be the session's own
//! tree, which is created with the session and dropped with it ([`ShellSession`]): both are
//! held HERE, beside the cluster, and handed to each program that runs on it.
//!
//! * The NODE's local disk is per node ([`ClusterRecord::node_disk`]): a bootstrap action runs
//!   once on every node, so what it writes is that node's, and a step runs on the cluster's
//!   primary node ([`ClusterRecord::primary_node_key`]), so what a step reads of the local disk
//!   is what the primary node holds. It is handed to a session as the files its tree starts
//!   with and taken back out of it afterwards ([`ClusterRecord::keep_node_disk`]).
//! * HDFS is the CLUSTER's — "the Hadoop Distributed File System … a distributed, scalable file
//!   system for Hadoop" that spans the cluster's nodes (Amazon EMR Management Guide) — so one
//!   tree is held per cluster ([`ClusterHdfs`]) and shared by every program of that cluster,
//!   which is what makes a write by one step a read by the next. It is handed to the Hadoop
//!   filesystem CLI ([`super::hadoop_cli`]) and reached by nothing else.
//!
//! Every path of both is a KEY of a tree this process holds in memory
//! ([`::service_common::posix_shell_volume`], whose bounds and path rules are stated there):
//! no host file, directory or path is opened, created, listed or removed anywhere on this path,
//! and both trees are bounded by this emulator's own numbers rather than by what a
//! caller-supplied program writes.

use ::std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ::service_common::posix_shell_session::ShellSession;
use ::service_common::posix_shell_volume::{parent_path, EntryFacts, ShellVolume, VolumeLimits};

use super::*;

/// How a Hadoop path names the cluster's own HDFS.
pub const HDFS_SCHEME: &str = "hdfs://";

/// How a Hadoop path names the NODE's local filesystem, rather than a distributed one.
pub const LOCAL_FILE_SCHEME: &str = "file://";

/// The directory an unqualified HDFS path is resolved against: the home directory of the user
/// an Amazon EMR cluster's commands run as (`hadoop`), which is what Hadoop resolves a relative
/// path against.
pub const HDFS_HOME_DIRECTORY: &str = "/user/hadoop";

/// The directories a cluster's HDFS holds as it comes up: the shared temporary directory, the
/// home directory of the cluster's own user (and the `/user` tree it lies in), and the
/// directory a cluster's own logs are written under.
const HDFS_SEEDED_DIRECTORIES: [&str; 4] = ["/tmp", "/user", HDFS_HOME_DIRECTORY, "/var/log"];

/// How much ONE cluster's HDFS holds.
///
/// CHOSEN, not an AWS limit: every byte in it was written by a caller-supplied program, so what
/// one cluster can keep in this process is bounded here rather than by the cluster's own node
/// storage. A write past a bound does not happen and the command performing it reports the
/// failure, the way a write to a full filesystem fails on a node.
const HDFS_LIMITS: VolumeLimits = VolumeLimits {
    files: 256,
    bytes: 512 * 1024,
    path_bytes: 1024,
    directories: 256,
    // HDFS holds files, not device nodes: nothing writes a device of this tree.
    device_lines: 0,
    device_line_bytes: 0,
};

/// How many files ONE node's local disk keeps between programs.
///
/// CHOSEN for the same reason [`HDFS_LIMITS`] is: these are the files a caller's bootstrap
/// script and its steps left behind, and what is kept beside the cluster is bounded here.
pub const MAX_NODE_DISK_FILES: usize = 128;

/// How many bytes those files come to.
pub const MAX_NODE_DISK_BYTES: usize = 256 * 1024;

/// ONE cluster's HDFS: the tree every program of that cluster reads and writes, shared so that
/// a write by one step is a read by the next.
///
/// Held behind a handle of its own rather than inside the cluster record's own guard, because
/// the programs that reach it run with no store guard held
/// ([`super::command_step_run`]): a command's own reads and writes happen while this
/// service's store is free, exactly as its object-store transfers do.
#[derive(Debug, Clone)]
pub struct ClusterHdfs {
    files: Arc<Mutex<ShellVolume>>,
}

impl Default for ClusterHdfs {
    /// The HDFS a cluster comes up with: an empty tree holding the directories a cluster's own
    /// HDFS has.
    fn default() -> Self {
        let mut files = ShellVolume::new(HDFS_LIMITS);
        for directory in HDFS_SEEDED_DIRECTORIES {
            files.make_directories(directory);
        }
        Self {
            files: Arc::new(Mutex::new(files)),
        }
    }
}

impl ClusterHdfs {
    /// The tree itself, for one operation on it.
    fn files(&self) -> MutexGuard<'_, ShellVolume> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What reading `path` answers, or `None` when HDFS holds no file there.
    #[must_use]
    pub fn read(&self, path: &str) -> Option<String> {
        self.files().read(path)
    }

    /// Write `contents` at `path`, with the directories above it made first.
    ///
    /// A filesystem's own `create` makes the parent directories of the file it creates, which
    /// is why a Hadoop write to a path whose directory does not exist yet leaves both behind.
    /// `false` is a write that did not happen — a path the tree does not accept, a directory,
    /// or a write past one of [`HDFS_LIMITS`] — which the command performing it reports as its
    /// own failure.
    pub fn write(&self, path: &str, contents: &str) -> bool {
        let mut files = self.files();
        if let Some(parent) = parent_path(path) {
            files.make_directories(&parent);
        }
        files.write(path, contents, false)
    }

    /// Make `path` a directory of HDFS, with every directory above it.
    pub fn make_directories(&self, path: &str) -> bool {
        self.files().make_directories(path)
    }

    /// Whether HDFS holds anything at `path`.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.files().exists(path)
    }

    /// Whether `path` is a directory of HDFS.
    #[must_use]
    pub fn is_directory(&self, path: &str) -> bool {
        self.files().is_directory(path)
    }

    /// What HDFS holds at `path`, as a listing of it reports that entry.
    #[must_use]
    pub fn facts(&self, path: &str) -> Option<EntryFacts> {
        self.files().facts(path)
    }

    /// The names directly under the directory `path`, in path order.
    #[must_use]
    pub fn entry_names(&self, path: &str) -> Vec<String> {
        self.files().entry_names(path)
    }

    /// Remove the file at `path`, answering whether there was one to remove.
    pub fn remove_file(&self, path: &str) -> bool {
        self.files().remove_file(path)
    }

    /// Remove everything at or under `path`, answering whether anything was removed — what a
    /// recursive removal of a directory does, performed by the tree itself
    /// ([`ShellVolume::remove_tree`]), so a directory the removal names really goes.
    pub fn remove_tree(&self, path: &str) -> bool {
        self.files().remove_tree(path)
    }

    /// The bytes every file at or under `path` holds, which is what a disk-usage walk of a
    /// directory of this filesystem answers.
    #[must_use]
    pub fn bytes_under(&self, path: &str) -> u64 {
        let under = format!("{}/", path.trim_end_matches('/'));
        self.files()
            .stored_files()
            .filter(|(held, _)| *held == path || held.starts_with(under.as_str()))
            .map(|(_, contents)| u64::try_from(contents.len()).unwrap_or_default())
            .sum()
    }

    /// Every file HDFS holds, as `(path, contents)` in path order — what a reader of the
    /// cluster's own filesystem sees, for the tests that assert what a step left behind.
    #[must_use]
    pub fn stored_files(&self) -> Vec<(String, String)> {
        self.files()
            .stored_files()
            .map(|(path, contents)| (path.to_string(), contents.to_string()))
            .collect()
    }
}

impl ClusterRecord {
    /// What the local disk of the node `node_key` holds, as the files a session's tree starts
    /// with.
    #[must_use]
    pub fn node_disk(&self, node_key: &str) -> Vec<(String, String)> {
        self.node_disks
            .get(node_key)
            .map(|files| {
                files
                    .iter()
                    .map(|(path, contents)| (path.clone(), contents.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep `files` as what the local disk of the node `node_key` holds, which is what the next
    /// program of that node reads.
    ///
    /// Bounded by [`MAX_NODE_DISK_FILES`] and [`MAX_NODE_DISK_BYTES`]: the files past either
    /// bound are not kept, so a caller-supplied program cannot grow this store without a stated
    /// ceiling.
    pub fn keep_node_disk(&mut self, node_key: &str, files: Vec<(String, String)>) {
        let mut kept: BTreeMap<String, String> = BTreeMap::new();
        let mut bytes = 0usize;
        for (path, contents) in files {
            if kept.len() >= MAX_NODE_DISK_FILES {
                break;
            }
            bytes = bytes.saturating_add(path.len().saturating_add(contents.len()));
            if bytes > MAX_NODE_DISK_BYTES {
                break;
            }
            kept.insert(path, contents);
        }
        if kept.is_empty() {
            self.node_disks.remove(node_key);
            return;
        }
        self.node_disks.insert(node_key.to_string(), kept);
    }

    /// What names the PRIMARY node of this cluster, under which its local disk is kept: the
    /// node a step's command line runs on.
    ///
    /// The same naming the bootstrap runs are recorded under ([`BootstrapNode::key`]), so the
    /// disk a bootstrap action of the primary node wrote to is the disk a step of that cluster
    /// reads.
    #[must_use]
    pub fn primary_node_key(&self, region: &str) -> String {
        self.bootstrap_nodes(region)
            .into_iter()
            .find(|node| node.is_master)
            .map_or_else(
                || instance_group_type::MASTER.to_string(),
                |node| node.key(),
            )
    }

    /// How many NODES this cluster runs on, which is what the replication its HDFS writes under
    /// follows from ([`super::hadoop_configuration`]).
    #[must_use]
    pub fn node_count(&self) -> usize {
        let launched = self
            .ec2_instances
            .iter()
            .filter(|instance| instance.state != instance_state::TERMINATED)
            .count();
        if launched > 0 {
            return launched;
        }
        // A cluster this emulator launched no instance for still runs its programs on one node
        // ([`ClusterRecord::bootstrap_nodes`]), so it is sized as that one node.
        1
    }
}

/// Seed `session` with the files the node's local disk holds, so the program it is about to
/// perform finds what the programs before it left there.
///
/// A file the session's tree will not take is not seeded, which is the tree's own bound rather
/// than anything the program decides.
pub fn seed_node_disk(session: &mut ShellSession, disk: &[(String, String)]) {
    for (path, contents) in disk {
        session.put_file(path, contents);
    }
}

/// The files `session` leaves on the node's local disk: what the program it performed wrote,
/// beside what it was seeded with and did not take away.
#[must_use]
pub fn kept_node_disk(session: &ShellSession) -> Vec<(String, String)> {
    session
        .tree_files()
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::ClusterHdfs;

    /// RULE: HDFS is the CLUSTER's, so what one program wrote another program of the same
    /// cluster reads, the write makes the directories above it, and a removal takes it away
    /// again.
    ///
    /// The rows differ along the shape of the path (a directory that exists, one the write has
    /// to make, a nested one) and along what is removed (one file, a whole subtree).
    #[test]
    fn what_one_program_writes_to_hdfs_another_reads() {
        let hdfs = ClusterHdfs::default();
        // (case, path, contents)
        let rows: [(&str, &str, &str); 3] = [
            ("a path under a directory the filesystem has", "/tmp/one.txt", "one"),
            (
                "a path whose directories the write makes",
                "/sim2real/run-1/two.txt",
                "two",
            ),
            (
                "a path under the cluster user's home directory",
                "/user/hadoop/three.txt",
                "three",
            ),
        ];
        for (case, path, contents) in rows {
            assert!(
                hdfs.write(path, contents),
                "case {case:?}: expected the write at {path} to happen"
            );
            // The SHARED handle is what another program of the cluster holds.
            let read = hdfs.clone().read(path);
            assert_eq!(
                read.as_deref(),
                Some(contents),
                "case {case:?}: expected {contents:?} back from {path}, got {read:?}"
            );
            let facts = hdfs.facts(path);
            assert_eq!(
                facts.map(|facts| facts.size),
                Some(contents.len()),
                "case {case:?}: expected {path} to report the bytes it holds"
            );
        }
        assert!(
            hdfs.remove_file("/tmp/one.txt"),
            "a file the filesystem holds is removed"
        );
        assert!(
            hdfs.read("/tmp/one.txt").is_none(),
            "case 'a removed file': expected nothing back, got {:?}",
            hdfs.read("/tmp/one.txt")
        );
        assert!(
            hdfs.remove_tree("/sim2real"),
            "a directory and everything under it are removed together"
        );
        assert!(
            hdfs.read("/sim2real/run-1/two.txt").is_none(),
            "case 'a removed subtree': expected nothing back under it"
        );
        assert!(
            !hdfs.exists("/sim2real") && !hdfs.exists("/sim2real/run-1"),
            "case 'a removed subtree': expected its directories to be gone with it"
        );
        assert!(
            hdfs.read("/user/hadoop/three.txt").is_some(),
            "case 'a removed subtree': expected the rest of the filesystem untouched"
        );
    }
}
