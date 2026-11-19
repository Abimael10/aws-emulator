// SPDX-License-Identifier: CC0-1.0

//! The Docker DAEMON a build environment carries: what the command that STARTS it answers.
//!
//! # The rule this module implements
//!
//! Every Docker command a buildspec runs is served by a daemon, and whether a build may run
//! one at all is what its environment's `privilegedMode` decides: that member "enables
//! running the Docker daemon inside a Docker container", and AWS documents a container build
//! as beginning by starting the daemon from its install phase — the managed images carry the
//! daemon's own program for exactly that. So the command that starts it is one a privileged
//! build environment performs, and one a build environment that was not started in privileged
//! mode cannot: that is the differential a client sees between the two kinds of environment,
//! and it is read off the build's own record rather than decided from the command's text.
//!
//! What SERVES the Docker commands of a build here is the engine [`super::docker_cli`] drives,
//! in this process and against the registries this emulator itself holds: a privileged
//! environment therefore has that engine from the moment the build starts, and the command
//! that starts the daemon has nothing left to do but report it. Nothing else about the daemon
//! is answered here — its own diagnostics and its lifecycle are the daemon's — and an
//! invocation carrying an operand is not this program's form at all.

use super::{answered, BuildTools, DOCKER_FAILED, PROGRAM_SUCCEEDED};
use ::service_common::posix_shell_session::ProgramOutcome;

/// The name a command line invokes the Docker daemon by.
pub const DOCKER_DAEMON_PROGRAM: &str = "dockerd";

/// What the daemon reports when it was asked to run in an environment it may not run in.
///
/// AWS publishes no sentence for it — a real build environment is a container CodeBuild
/// starts, and what its daemon writes when the container is unprivileged is that daemon's —
/// so this states the condition in this service's own terms, naming the setting that decides
/// it, the way the environment states one it could not provision.
const NOT_PRIVILEGED_MESSAGE: &str =
    "failed to start daemon: the Docker daemon runs only in a build environment whose \
     privilegedMode is enabled";

/// Perform the invocation `arguments` asks for, or answer `None` for one this program
/// performs nothing of.
///
/// The daemon takes OPTIONS and no operand, and NONE of them is read: which storage driver
/// it would use and which socket it would listen on are the configuration of a daemon this
/// environment runs in process and takes from nothing a command line says, so the options
/// are the ones of the daemon already serving this build. A line whose first word is not an
/// option is naming an operand the daemon has none of, which is no invocation of this
/// program.
pub(super) fn performed(tools: &BuildTools, arguments: &[String]) -> Option<ProgramOutcome> {
    if arguments
        .first()
        .is_some_and(|word| !word.starts_with('-'))
    {
        return None;
    }
    if !tools.privileged {
        return Some(tools.refused(
            DOCKER_DAEMON_PROGRAM,
            NOT_PRIVILEGED_MESSAGE,
            DOCKER_FAILED,
        ));
    }
    // The engine this build's Docker commands are served by is already there.
    Some(answered(PROGRAM_SUCCEEDED, Vec::new()))
}
