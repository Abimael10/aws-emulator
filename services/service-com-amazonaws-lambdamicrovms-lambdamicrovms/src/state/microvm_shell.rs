// SPDX-License-Identifier: CC0-1.0

//! The interactive SHELL of one MicroVM: what a shell authentication token grants, and
//! what a command line run inside a MicroVM answers.
//!
//! # What a shell token is for
//!
//! `CreateMicrovmShellAuthToken` "creates a shell authentication token for interactive
//! shell access to a running MicroVM", and "the MicroVM must have been run with the
//! SHELL_INGRESS network connector attached"
//! ([`super::SHELL_INGRESS_NETWORK_CONNECTOR`]). So a shell token is a token of the
//! MicroVM's published endpoint ([`super::microvm_endpoint`], the one address a MicroVM
//! is reached on) that grants its SHELL rather than a port of the server its guest bound:
//! the two kinds of token the API mints are the two things an endpoint carries
//! ([`super::microvm_endpoint::TokenGrant`]), and each grants only its own.
//!
//! # What running a line in a MicroVM does
//!
//! The shell is the bounded, in-process POSIX session this workspace performs every
//! caller-supplied shell program in (`::service_common::posix_shell_session`, whose module
//! docs state the catalog it performs and why it is a session rather than a host process),
//! run over the MicroVM's OWN machine — the tree its guest reads and writes, the
//! environment its image version declares and the identity its kernel published
//! ([`super::guest_machine`]). A command that writes a file writes the MicroVM's disk, so
//! the guest's own program reads what the shell wrote and the next shell command reads it
//! too; a command the session performs no program for runs nothing and answers the status
//! a shell answers for a utility it could not find.
//!
//! Nothing here starts a process, opens a host path or reaches a socket: the tree is the
//! MicroVM's, in this process's memory, and a path it does not hold resolves to nothing.
//!
//! # The form the channel takes
//!
//! AWS publishes no wire protocol for the shell, so what this emulator serves on the
//! MicroVM's published endpoint for a request presenting a shell token is the plainest
//! form of the channel: the request's BODY is the program to perform, one command per
//! line, and the answer carries the lines the shell wrote — standard output and standard
//! error in the order the session wrote them, which is what a terminal shows — with the
//! status the program ended on in [`SHELL_STATUS_HEADER`]. A MicroVM that is not RUNNING
//! its guest has no shell to reach, which the endpoint answers for as it does for a guest
//! it reached nothing inside of.

use ::service_common::posix_shell_session::{
    Bounds, OutputLimits, SessionConfig, ShellSession, Statuses,
};

use super::guest_machine::GuestMachine;

/// The header the answer to a shell request reports the status the program ended on in.
///
/// Named beside the endpoint's own headers ([`super::microvm_endpoint::AUTH_TOKEN_HEADER`],
/// which AWS documents, and `X-aws-proxy-port`): the shell channel has no published wire
/// form, so the status a shell reports of its program is carried here rather than folded
/// into the HTTP status, which is the PROXY's answer about reaching the MicroVM at all.
pub const SHELL_STATUS_HEADER: &str = "X-aws-proxy-shell-status";

/// How many command lines ONE shell request performs.
///
/// The program is caller-supplied text, so what one request can cost is bounded here —
/// well above an interactive line or a short script, and far below what would cost this
/// process anything noticeable. A line past the budget performs nothing and answers as a
/// command that could not run ([`Statuses::could_not_run`]).
pub const MAX_SHELL_COMMANDS: usize = 64;

/// How many bytes of the request the program is read from.
///
/// The body is the caller's own text, so what one request may hand the shell is bounded
/// here as well as line by line ([`Bounds::command_bytes`]) and command by command
/// ([`MAX_SHELL_COMMANDS`]); the bytes past it are not part of the program.
pub const MAX_SHELL_PROGRAM_BYTES: usize = 64 * 1024;

/// How many lines of what a shell program wrote one answer carries.
pub const MAX_SHELL_OUTPUT_LINES: usize = 256;

/// How many bytes ONE of those lines carries; a longer line is truncated rather than
/// dropped, so what the program wrote is still reported.
pub const MAX_SHELL_OUTPUT_LINE_BYTES: usize = 4 * 1024;

/// What one shell program answered: the lines it wrote and the status it ended on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellAnswer {
    /// The lines the program wrote, in the order it wrote them — what a terminal shows.
    pub output: Vec<String>,
    /// The status the program ended on: the shell's own, zero for a program that ran to
    /// success and [`Statuses::could_not_run`] for a command this MicroVM has no program
    /// for.
    pub status: i32,
}

/// The session one shell request is performed in: the whole command catalog, this
/// request's own budget, and the lines it keeps.
fn shell_config() -> SessionConfig {
    SessionConfig {
        bounds: Bounds {
            commands: MAX_SHELL_COMMANDS,
            ..Bounds::default()
        },
        output: Some(OutputLimits {
            lines: MAX_SHELL_OUTPUT_LINES,
            line_bytes: MAX_SHELL_OUTPUT_LINE_BYTES,
        }),
        ..SessionConfig::default()
    }
}

/// Perform `program` in the shell of `machine` — the MicroVM's own machine — and answer
/// what it wrote and the status it ended on.
///
/// The lines run in ONE session, so a variable one line sets is there for the next, and
/// the files the session leaves behind are the MicroVM's disk from then on
/// ([`GuestMachine::in_volume`]). A line that ends the session (`exit`) ends the program,
/// as a script's own `exit` does, and that status is the program's.
#[must_use]
pub fn run_in_machine(machine: &GuestMachine, program: &str) -> ShellAnswer {
    // Only the first [`MAX_SHELL_PROGRAM_BYTES`] of the request are the program, so the
    // work this reads is bounded before any of it is performed. The split is at a
    // character boundary, so the text stays the text the caller wrote.
    let program = match program
        .char_indices()
        .find(|(index, _)| *index >= MAX_SHELL_PROGRAM_BYTES)
    {
        Some((index, _)) => program.get(..index).unwrap_or_default(),
        None => program,
    };
    let lines: Vec<&str> = program
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(MAX_SHELL_COMMANDS)
        .collect();
    machine.in_volume(|tree| {
        let mut session = ShellSession::in_volume(machine.env.clone(), shell_config(), tree);
        let mut status = Statuses::default().success;
        for line in lines {
            status = session.run(line);
            if let Some(ended) = session.exit_status() {
                status = ended;
                break;
            }
        }
        let answer = ShellAnswer {
            output: session.output().to_vec(),
            status,
        };
        // The tree the session ran in, with everything the program wrote to it: it is the
        // MicroVM's disk, so the machine takes it back.
        let left = session.into_volume();
        (answer, left)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use super::*;

    /// A MicroVM's machine, as one that has just reached `RUNNING` holds it.
    fn machine() -> GuestMachine {
        let now = Instant::now();
        GuestMachine::boot(
            "11111111-2222-4333-8444-555555555555".to_string(),
            "mvm-0123456789abcdef".to_string(),
            None,
            "ARM_64",
            BTreeMap::from([("MARKER".to_string(), "a-value".to_string())]),
            now,
            now,
        )
    }

    /// RULE: a shell program is PERFORMED in the MicroVM's own machine — the status it
    /// answers is the shell's own for what the line did, and a command the session has no
    /// program for runs nothing.
    #[test]
    fn a_program_answers_the_status_the_shell_answers_for_it() {
        let machine = machine();
        // (case, the program, the status it ends on)
        let rows: Vec<(&str, &str, i32)> = vec![
            ("a command that succeeded", "echo one", 0),
            ("a command that reported failure", "false", 1),
            ("a command this machine has no program for", "systemctl", 127),
            ("a line that does not parse", "echo one |", 2),
            ("an and-or list the first command ended", "false && echo two", 1),
            ("a program ending itself", "exit 7", 7),
        ];
        for (case, program, expected) in rows {
            let answered = run_in_machine(&machine, program);
            assert_eq!(
                answered.status, expected,
                "case {case:?}: expected status {expected}, got {answered:?}"
            );
        }
    }

    /// RULE: what a shell command writes to the machine is the MicroVM's disk, so a later
    /// command of a later request reads it back — and the environment the program sees is
    /// the one its image declared.
    #[test]
    fn what_one_command_wrote_is_there_for_the_next_request() {
        let machine = machine();
        let wrote = run_in_machine(&machine, "echo kept > /data/note.txt");
        assert_eq!(
            wrote.status, 0,
            "the write must succeed, got {wrote:?}"
        );
        let read = run_in_machine(&machine, "cat /data/note.txt");
        assert_eq!(
            read.output,
            vec!["kept".to_string()],
            "a later request reads what the earlier one wrote, got {read:?}"
        );
        // The machine's own reader sees the same file: the tree the session ran in IS the
        // MicroVM's disk rather than a copy of it.
        assert_eq!(
            machine.read("/data/note.txt").as_deref(),
            Some("kept\n"),
            "the file is the machine's, got {:?}",
            machine.read("/data/note.txt")
        );
        let declared = run_in_machine(&machine, "echo $MARKER");
        assert_eq!(
            declared.output,
            vec!["a-value".to_string()],
            "the program sees the environment the image declared, got {declared:?}"
        );
    }

    /// RULE: what the kernel published to the guest is readable from its shell, and a
    /// path the machine holds nothing at reads nothing.
    #[test]
    fn the_shell_reads_what_the_machine_published() {
        let machine = machine();
        let boot = run_in_machine(
            &machine,
            &format!("cat {}", super::super::guest_machine::BOOT_ID_PATH),
        );
        assert_eq!(
            boot.output,
            vec![machine.boot_id.clone()],
            "the shell reads this boot's id, got {boot:?}"
        );
        let absent = run_in_machine(&machine, "cat /data/absent.txt");
        assert_ne!(
            absent.status, 0,
            "a path the machine holds nothing at reads nothing, got {absent:?}"
        );
    }

    /// RULE: one request performs at most [`MAX_SHELL_COMMANDS`] lines, so a program
    /// longer than the budget does not run unbounded work.
    #[test]
    fn a_program_is_bounded_by_the_requests_own_budget() {
        let machine = machine();
        let program = (0..MAX_SHELL_COMMANDS * 2)
            .map(|line| format!("echo line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let answered = run_in_machine(&machine, &program);
        assert!(
            answered.output.len() <= MAX_SHELL_COMMANDS,
            "expected at most {MAX_SHELL_COMMANDS} lines performed, got {}",
            answered.output.len()
        );
    }
}
