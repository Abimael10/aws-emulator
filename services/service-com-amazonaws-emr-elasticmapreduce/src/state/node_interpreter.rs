// SPDX-License-Identifier: CC0-1.0

//! The PYTHON INTERPRETER a cluster node's command line runs a program with: `python3`.
//!
//! "command-runner.jar … lets you run commands on your cluster" (Amazon EMR documentation),
//! and the universal shape of such a command is a shell line that ends in a Python program:
//! the line copies a helper out of Amazon S3 with the AWS CLI, runs `python3 /tmp/<helper>.py`
//! over it and carries the result back. An Amazon EMR release ships that interpreter on every
//! node (its Python-backed applications are built on it), so a command line that invokes it is
//! one the node really performs — and a session that provided no such program would answer the
//! status a shell gives a command it could not find, which is no answer about the submitted
//! work.
//!
//! # Why it is reached rather than written here
//!
//! The one `python3` CLI this workspace has is the Systems Manager crate's
//! ([`::service_com_amazonaws_ssm_amazonssm::state::guest_python`]), which installs it into
//! the session a managed node's command runs in: it reads the invocation off the command's own
//! words (the program on `-c`, a file of the session's tree, the module `-m` names, a program
//! on standard input), hands the program the variables the session holds and what the command
//! reads on standard input, and reports the two streams and the status it ended on. A cluster
//! node's command line needs exactly that program, so this module reaches that one rather than
//! standing a second interpreter CLI up here.
//!
//! Everything that CONTAINS and BOUNDS a run therefore lives where it already does: the one
//! contained Python engine this workspace embeds (the Lambda crate's
//! `state::execution::python`, which this crate already runs a submitted Spark application in
//! — [`super::spark_step_run`]), with its native modules an allow-list, every way of opening a
//! host file taken away before a line of a program is read, and the deadline, step, memory,
//! recursion and output ceilings enforced in host code no program can reach. Nothing here
//! starts a process, opens a host file or reaches a socket.
//!
//! # What ONE command's programs may spend
//!
//! The allowance those programs share is taken per COMMAND
//! ([`::service_com_amazonaws_ssm_amazonssm::state::guest_python::ProgramAllowance::for_one_request`]):
//! one step's command line is one piece of submitted work, so how many programs it may run and
//! how much of this host's own time they may take between them follows from that allowance
//! rather than from how many times the line invokes the interpreter.

use ::service_com_amazonaws_ssm_amazonssm::state::guest_python::{
    interpreter_programs, ProgramAllowance,
};
use ::service_common::posix_shell_session::EnvironmentPrograms;

/// The interpreter one command line of a cluster node is given, with an allowance of its own.
#[must_use]
pub fn node_python_interpreter() -> Box<dyn EnvironmentPrograms> {
    interpreter_programs(&ProgramAllowance::for_one_request())
}
