// SPDX-License-Identifier: CC0-1.0

//! The build environment's SHELL: what ONE command line of a buildspec does.
//!
//! # The rule this module implements
//!
//! A build's phase commands are command LINES, which CodeBuild hands to the shell of the
//! build environment; what the build reports for a phase is the status that shell answered
//! with — zero for a command that ran to success, non-zero otherwise — so the outcome of a
//! build follows from running its own commands and from nothing else.
//!
//! # What runs them
//!
//! [`::service_common::posix_shell_session`], the workspace's ONE bounded, session-local
//! POSIX/bash session: the grammar is `brush-parser`'s, the catalog of utilities it performs
//! is session-local, every other command and every command FORM and expansion it does not
//! perform fails closed with the status a shell answers for a command it cannot run, and all
//! of its bounds — the command budget, the line size, the nesting count, the expansion sizes
//! and the variable store — are stated there. That module also states why the project's
//! EXECUTING half, `brush-core`, is not a dependency of this workspace.
//!
//! A build's session is that session with the statuses POSIX gives those conditions, the
//! shared bounds, and what its commands WROTE kept for the build's log: the bytes a command
//! writes are what a build's log group carries ([`super::build_run`] delivers them), so this
//! session keeps them, bounded by [`MAX_LOG_LINES`] and [`MAX_LOG_LINE_BYTES`]. What a
//! performed command contributes to the build is its exit status, the lines it wrote, the
//! variables it sets (a buildspec's `env/exported-variables` are read back off this session)
//! and the files it writes into the session's own working tree, which the commands after it
//! read, and which is what [`BuildShell`] exposes.
//!
//! That working tree is the build environment's own storage — the source directory a build
//! runs in — so the session is given the file utilities over it
//! ([`::service_common::posix_shell_session::ShellSession::with_file_utilities`]): the
//! service SEEDS it with the files the build is to find there (a source archive it
//! downloaded, a path it restored from the cache) and takes back out what the commands
//! produced, which is what the build's artifacts and its cache are selected from.
//!
//! The commands the build ENVIRONMENT provides beyond the shell's own utilities are the
//! programs of the runtimes this build installed ([`super::runtime_versions`]) and the TOOLS
//! the managed image carries ([`super::build_tools`]) — together [`super::BuildPrograms`],
//! which the session asks for a command its catalog does not name.
//!
//! What the environment already HOLDS when the first command runs is the IMAGE's own
//! filesystem ([`super::build_image`]), seeded into the same tree: the build runs inside
//! that image, so its files are there to be read. They are the environment's rather than
//! the build's output, so [`BuildShell::files`] leaves them out of what the run takes back
//! out of the tree.

use std::collections::BTreeMap;

/// How many lines of what its commands wrote ONE build keeps for its log.
///
/// CHOSEN, not an AWS limit: the lines are kept in this process until the build's log is
/// delivered, so how many one build may keep is bounded here. Past it a line is not kept.
pub const MAX_LOG_LINES: usize = 2_048;

/// How many bytes one kept line carries; a longer line is truncated rather than dropped, so
/// what the command wrote is still in the log. CHOSEN for the same reason as
/// [`MAX_LOG_LINES`].
pub const MAX_LOG_LINE_BYTES: usize = 16 * 1024;

/// How many seconds of `sleep` ONE build's commands may state they took, in total.
///
/// A build runs no longer than the longest execution timeout it may be given:
/// `Build$timeoutInMinutes` is "How long, in minutes, from 5 to 2160 (36 hours), for
/// CodeBuild to wait before timing out this build if it does not get marked as completed",
/// so a build is already over at 2160 minutes and no phase of it occupies the time past
/// that. The session states no time beyond this
/// ([`::service_common::posix_shell_session::Bounds::sleep_seconds`]).
pub const MAX_SLEEP_SECONDS: u64 = 2_160 * 60;

/// The session one build's commands run in: the shared session, configured as a build's,
/// with the programs the runtimes this build installed provide.
#[derive(Debug)]
pub(super) struct BuildShell {
    session: ::service_common::posix_shell_session::ShellSession,
    /// The paths the build's IMAGE presents in the environment's filesystem, which the
    /// build itself did not produce.
    image_paths: Vec<String>,
}

impl BuildShell {
    /// A session holding `variables`, with `programs`' programs, `image_files` already in
    /// its tree and the build's whole command budget left.
    pub(super) fn new(
        variables: BTreeMap<String, String>,
        programs: super::BuildPrograms,
        image_files: &[(String, String)],
    ) -> Self {
        let mut shell = Self {
            session: ::service_common::posix_shell_session::ShellSession::with_programs(
                variables,
                // The shared statuses and bounds, and the lines its commands write kept for
                // the build's log: see this module's header.
                ::service_common::posix_shell_session::SessionConfig {
                    output: Some(::service_common::posix_shell_session::OutputLimits {
                        lines: MAX_LOG_LINES,
                        line_bytes: MAX_LOG_LINE_BYTES,
                    }),
                    // A build REPORTS how long each of its phases took, so the time its
                    // commands state they took is time this session may state
                    // ([`MAX_SLEEP_SECONDS`]).
                    bounds: ::service_common::posix_shell_session::Bounds {
                        sleep_seconds: MAX_SLEEP_SECONDS,
                        ..::service_common::posix_shell_session::Bounds::default()
                    },
                    ..::service_common::posix_shell_session::SessionConfig::default()
                },
                Box::new(programs),
            )
            // The build environment has a working directory of its own, so the file
            // utilities are commands this session performs over it.
            .with_file_utilities(),
            image_paths: Vec::new(),
        };
        for (path, contents) in image_files {
            // A file the tree could not hold is one the commands will not find, and it is
            // not claimed to be there either.
            if shell.session.put_file(path, contents) {
                shell.image_paths.push(path.clone());
            } else {
                ::tracing::debug!(
                    %path,
                    "a build's working tree could not hold a file of the image it runs in"
                );
            }
        }
        shell
    }

    /// Seed a file the build is to find in its working tree, answering whether the tree
    /// took it — what an unpacked source archive and a restored cache path arrive through.
    pub(super) fn put_file(&mut self, path: &str, contents: &str) -> bool {
        self.session.put_file(path, contents)
    }

    /// The files this build's working tree holds, in path order: what its commands
    /// produced, which is what its artifacts and its cache are selected from.
    ///
    /// The files the IMAGE presents are not among them: they are the environment the build
    /// ran in rather than output the build produced, so nothing selects an artifact or a
    /// cached path out of them.
    pub(super) fn files(&self) -> Vec<(&str, &str)> {
        self.session
            .tree_files()
            .into_iter()
            .filter(|(path, _)| !self.image_paths.iter().any(|image| image == path))
            .collect()
    }

    /// Forget an `exit` that ended one of this build's command blocks, so the block the
    /// build runs next — a phase's `finally` block — runs as the program it is.
    pub(super) fn clear_exit(&mut self) {
        self.session.clear_exit();
    }

    /// The value this session holds for `name`, or `None` when it holds none.
    pub(super) fn variable(&self, name: &str) -> Option<&str> {
        self.session.variable(name)
    }

    /// The status `exit` ended this session with, or `None` while nothing has.
    pub(super) fn exit_status(&self) -> Option<i32> {
        self.session.exit_status()
    }

    /// How long this build's commands have stated they took so far, in milliseconds.
    ///
    /// The session performs the commands in process and spends none of the host's clock on
    /// them, so what a command states it took is the time it took
    /// ([`::service_common::posix_shell_session::ShellSession::slept`]) — and a build
    /// REPORTS that time, as the window each of its phases occupied. Saturating: a session
    /// may state no more than [`MAX_SLEEP_SECONDS`], which is far inside this range.
    pub(super) fn slept_millis(&self) -> i64 {
        i64::try_from(self.session.slept().as_millis()).unwrap_or(i64::MAX)
    }

    /// Run one command LINE and answer the status the shell answered for it.
    pub(super) fn run(&mut self, line: &str) -> i32 {
        self.session.run(line)
    }

    /// What this build's commands WROTE, as the lines the build's log carries: standard
    /// output and then standard error, each in the order the commands wrote it.
    ///
    /// The session keeps the two streams apart (a diagnostic is not what a redirection takes
    /// or a substitution captures), and a log carries both, so the delivery joins them here.
    pub(super) fn written(&self) -> Vec<String> {
        self.session
            .output()
            .iter()
            .chain(self.session.errors())
            .cloned()
            .collect()
    }
}
