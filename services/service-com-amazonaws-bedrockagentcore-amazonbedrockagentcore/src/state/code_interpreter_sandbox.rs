// SPDX-License-Identifier: CC0-1.0

//! The ONE filesystem a code interpreter session's three views share.
//!
//! A code interpreter sandbox has three ways in — the file tools (`writeFiles`, `readFiles`,
//! `listFiles`, `removeFiles`), the code a run executes, and the commands a shell performs —
//! and all three address ONE filesystem: a file one of them wrote is a file the other two
//! read. That filesystem is [`super::CodeInterpreterSessionRecord::files`], held in this
//! process's memory; this module is how a RUN (a script or a command, neither of which may
//! hold the service's state guard while it runs) reaches it.
//!
//! A run is handed a [`SandboxFilesystem`]: the session's files as they were when the run
//! started, plus a record of what the run itself wrote and removed. While the run lasts every
//! read and write it makes lands here, and when it ends the record is applied back to the
//! session ([`SandboxFilesystem::apply_to`]) under the state guard, against the same
//! workspace ceilings a `writeFiles` call is weighed by. A path is a workspace KEY
//! ([`super::normalize_session_path`]) in both directions, so neither a script nor a command
//! can name anything outside the session's own state, and no host path is ever opened.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use super::{
    normalize_session_path, CodeInterpreterSessionRecord, PathRejection, MAX_SESSION_FILES,
    MAX_SESSION_FILE_BYTES,
};

/// Why a sandbox filesystem call did not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxFileError {
    /// The path addresses nothing in the session workspace.
    Path(PathRejection),
    /// There is no file at that path.
    NotFound,
    /// The path names a directory of the workspace rather than a file in it.
    IsDirectory,
    /// The workspace holds as much as it may ([`MAX_SESSION_FILES`],
    /// [`MAX_SESSION_FILE_BYTES`]).
    Full,
}

/// The session workspace as ONE run sees it, and what that run changed in it.
///
/// Reads answer out of [`Self::files`], which is the session's workspace as it was when the
/// run started; a write or a removal changes that map AND is recorded, so what the run did is
/// applied to the live session afterwards rather than from inside the run (which holds no
/// state guard). The per-session ceilings are checked here as well as on the way back, so a
/// run cannot grow this process's memory past them while it lasts.
#[derive(Debug, Default)]
pub struct SandboxFilesystem {
    /// Every file of the workspace, keyed as [`normalize_session_path`] spells it.
    files: BTreeMap<String, Vec<u8>>,
    /// The keys this run WROTE, so the session takes exactly those back.
    written: BTreeSet<String>,
    /// The keys this run REMOVED, for the same reason.
    removed: BTreeSet<String>,
}

impl SandboxFilesystem {
    /// The workspace of `session`, as the run about to start sees it.
    #[must_use]
    pub fn of_session(session: &CodeInterpreterSessionRecord) -> Self {
        Self {
            files: session
                .files
                .iter()
                .map(|(key, file)| (key.clone(), file.bytes.clone()))
                .collect(),
            written: BTreeSet::new(),
            removed: BTreeSet::new(),
        }
    }

    /// The bytes the workspace holds at `path`.
    pub fn read(&self, path: &str) -> Result<&[u8], SandboxFileError> {
        let key = normalize_session_path(path).map_err(SandboxFileError::Path)?;
        match self.files.get(&key) {
            Some(bytes) => Ok(bytes),
            None if self.holds_directory(&key) => Err(SandboxFileError::IsDirectory),
            None => Err(SandboxFileError::NotFound),
        }
    }

    /// Put `bytes` at `path`, replacing what the workspace held there.
    pub fn write(&mut self, path: &str, bytes: Vec<u8>) -> Result<(), SandboxFileError> {
        let key = normalize_session_path(path).map_err(SandboxFileError::Path)?;
        if self.holds_directory(&key) {
            return Err(SandboxFileError::IsDirectory);
        }
        let replaced = self.files.get(&key).map_or(0, Vec::len);
        if !self.files.contains_key(&key) && self.files.len() >= MAX_SESSION_FILES {
            return Err(SandboxFileError::Full);
        }
        let after = self
            .bytes()
            .saturating_sub(replaced)
            .saturating_add(bytes.len());
        if after > MAX_SESSION_FILE_BYTES {
            return Err(SandboxFileError::Full);
        }
        self.files.insert(key.clone(), bytes);
        self.removed.remove(&key);
        self.written.insert(key);
        Ok(())
    }

    /// Drop the file at `path`.
    pub fn remove(&mut self, path: &str) -> Result<(), SandboxFileError> {
        let key = normalize_session_path(path).map_err(SandboxFileError::Path)?;
        if self.files.remove(&key).is_none() {
            return Err(SandboxFileError::NotFound);
        }
        self.written.remove(&key);
        self.removed.insert(key);
        Ok(())
    }

    /// Whether the workspace holds a file at `path`.
    #[must_use]
    pub fn holds(&self, path: &str) -> bool {
        normalize_session_path(path).is_ok_and(|key| self.files.contains_key(&key))
    }

    /// Whether some file of the workspace lies UNDER `key`, which is what makes that key a
    /// directory: the workspace keys a file by its whole path and holds no entry of its own
    /// for a directory, so a directory exists exactly as long as a file under it does.
    #[must_use]
    pub fn holds_directory(&self, key: &str) -> bool {
        if key.is_empty() {
            return true;
        }
        let prefix = format!("{key}/");
        self.files.keys().any(|held| held.starts_with(&prefix))
    }

    /// The names directly under the directory `path` names (the workspace root for the empty
    /// path), each once and in path order — what a listing of that directory answers.
    #[must_use]
    pub fn entries_under(&self, path: &str) -> Vec<String> {
        let key = match normalize_session_path(path) {
            Ok(key) => key,
            Err(PathRejection::Empty) => String::new(),
            Err(_) => return Vec::new(),
        };
        let prefix = if key.is_empty() {
            String::new()
        } else {
            format!("{key}/")
        };
        let mut names: BTreeSet<String> = BTreeSet::new();
        for held in self.files.keys() {
            let Some(relative) = held.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let name = match relative.split_once('/') {
                Some((child, _)) => child.to_string(),
                None => relative.to_string(),
            };
            if !name.is_empty() {
                names.insert(name);
            }
        }
        names.into_iter().collect()
    }

    /// Every file of this filesystem, as `(key, bytes)` in key order: what a caller seeds
    /// another view of the same workspace from.
    #[must_use]
    pub fn files(&self) -> Vec<(String, Vec<u8>)> {
        self.files
            .iter()
            .map(|(key, bytes)| (key.clone(), bytes.clone()))
            .collect()
    }

    /// How many bytes this filesystem holds across every file in it.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.files.values().map(Vec::len).sum()
    }

    /// Whether this run changed the workspace at all.
    #[must_use]
    pub fn changed(&self) -> bool {
        !self.written.is_empty() || !self.removed.is_empty()
    }

    /// Write back to `session` exactly what the run wrote and removed, at `now`.
    ///
    /// Only the keys the run TOUCHED are applied, so a write another call made to this
    /// session while the run was going is left where it is rather than rolled back to the
    /// snapshot this run was handed. Every write is weighed by the session's own ceilings
    /// ([`CodeInterpreterSessionRecord::write_file`]), so a run cannot store what a
    /// `writeFiles` call could not; a write the workspace has no room for is dropped, and
    /// the count of dropped writes is answered so the caller can report it.
    pub fn apply_to(
        &self,
        session: &mut CodeInterpreterSessionRecord,
        now: SystemTime,
        bytes_elsewhere: usize,
    ) -> usize {
        let mut refused = 0usize;
        for key in &self.removed {
            session.files.remove(key);
        }
        for key in &self.written {
            let Some(bytes) = self.files.get(key) else {
                continue;
            };
            if session
                .write_file(key, bytes.clone(), now, bytes_elsewhere)
                .is_err()
            {
                refused = refused.saturating_add(1);
            }
        }
        refused
    }
}
