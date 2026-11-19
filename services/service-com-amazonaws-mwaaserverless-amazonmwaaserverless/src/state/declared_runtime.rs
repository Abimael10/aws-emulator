// SPDX-License-Identifier: CC0-1.0

//! How long the work ONE task of a workflow definition DECLARES takes.
//!
//! A run "executes the tasks defined in your workflow definition"
//! (`smithy_query(query="doc", shape="CreateWorkflowRequest",
//! member="DefinitionS3Location")`), and what a run REPORTS about that execution is a
//! window: `WorkflowRunDetail$StartedOn` is "The timestamp when the workflow run started
//! execution", `WorkflowRunDetail$CompletedOn` "The timestamp when the workflow run
//! completed execution" and `WorkflowRunDetail$Duration` "The total duration of the
//! workflow run execution in seconds" (`smithy_query(query="shape",
//! shape="WorkflowRunDetail")`). A window that describes an execution has to be the
//! window THAT work occupies, so the span a task occupies is read out of the task's own
//! declaration rather than being a constant: a definition declaring a task that waits
//! half a minute has a run that is still executing half a minute later, and one declaring
//! a task that waits a moment has a run that is finished.
//!
//! What a declaration states about its own duration is what the command line it declares
//! states: an Apache Airflow task's command is a shell program, and the time a program
//! states it took is what its `sleep` commands asked for.
//!
//! Nothing of that is read here. The program is performed by
//! [`::service_common::posix_shell_session`], this workspace's ONE bounded, session-local
//! POSIX/bash session: it parses the line with the shell's own grammar, performs a fixed
//! catalog of session-local utilities and fails every other command and every form it does
//! not perform closed, has no process to start, no network to reach and no host filesystem,
//! and states the time a program took without any thread waiting for it
//! ([`::service_common::posix_shell_session::ShellSession::slept`]) — which is the same
//! answer a build's phase duration is taken from. So nothing here runs a command, nothing
//! is slept on, and the shell's own forms (an and-or list, a pipeline, quoting, an
//! assignment before the command, a nested `sh -c` program) are the grammar's rather than
//! something scanned out of the text. The run's own schedule then carries the stated time
//! on the clock ([`crate::state::WorkflowRun::settle`]).
//!
//! A program that states no time — one with no `sleep` in it, or one whose `sleep` the
//! shell refused — declares no duration, and the run's default span covers it
//! ([`crate::state::TASK_LEVEL_RUNS_FOR`]). A program is performed one LINE at a time, the
//! session's own unit of a program, so a declaration written over several lines states the
//! time of each of them; a construct that SPANS lines is not one line and so is not read
//! as one, which is the session's closed failure and therefore states no time either.

use std::collections::BTreeMap;
use std::time::Duration;

/// The longest time one task of a definition may state it took, which the session itself
/// holds its total to ([`::service_common::posix_shell_session::Bounds::sleep_seconds`]).
///
/// The ceiling is the emulator's own, not a published service limit: it is what keeps the
/// instant a run's next change falls due a value this process can compute, however large a
/// number a caller's definition writes after `sleep`. It is set a long way past any wait a
/// test or a caller polling a run can be waiting on.
pub const MAX_DECLARED_RUNTIME: Duration = Duration::from_secs(24 * 60 * 60);

/// How long the task whose command line is `command` occupies, and `None` for a program
/// that states no time at all.
pub fn declared_runtime(command: &str) -> Option<Duration> {
    let mut session = ::service_common::posix_shell_session::ShellSession::new(
        // A task's command line is given no variables by this service: what the reader
        // needs of the program is the time it states it took.
        BTreeMap::new(),
        ::service_common::posix_shell_session::SessionConfig {
            bounds: ::service_common::posix_shell_session::Bounds {
                sleep_seconds: MAX_DECLARED_RUNTIME.as_secs(),
                ..::service_common::posix_shell_session::Bounds::default()
            },
            // Nothing here reads what the program WROTE — a run's output is the lines its
            // own execution produced (`crate::state::run_logs`) — so no session keeps it.
            ..::service_common::posix_shell_session::SessionConfig::default()
        },
    )
    // An Apache Airflow task runs on a worker that has a working directory, so the file
    // utilities — and `sleep` itself, and a nested `sh -c` program — are commands this
    // session performs over its own session-local tree.
    .with_file_utilities();
    // A declared command is a PROGRAM, so each of its lines is performed in the one
    // session, in order; the session's own command budget bounds the whole of it.
    for line in command.lines() {
        session.run(line);
    }
    let stated = session.slept();
    (stated > Duration::ZERO).then_some(stated)
}
