// SPDX-License-Identifier: CC0-1.0

//! The SANDBOX's shell: what ONE command submitted to a sandbox does.
//!
//! # The rule this module implements
//!
//! A sandbox is a LIVE session of a project's environment, and a command execution
//! submitted to it is run in that session: what the execution reports follows from
//! running the command and from nothing else — `CommandExecution$exitCode` is "The exit
//! code to return upon completion", `standardOutputContent` "The text written by the
//! command to stdout" and `standardErrContent` "The text written by the command to
//! stderr" — so the execution's status is `SUCCEEDED` for a command the shell answered
//! zero for and `FAILED` for one it did not.
//!
//! ONE session per sandbox, for as long as the store holds that sandbox: a sandbox runs
//! the commands submitted to it one after another in the same environment, so a variable
//! one command set, and a file it wrote, are what the next command of that sandbox finds.
//! The sandbox is the environment; a command execution is one program run in it.
//!
//! # What runs them
//!
//! [`::service_common::posix_shell_session`], the workspace's ONE bounded, session-local
//! POSIX/bash session — the same session a build's phase commands run in
//! ([`super::build_shell`]). That module states what it is and what it cannot do, and both
//! matter here, because the command is a caller's text:
//!
//! * it runs IN THIS PROCESS and has no process to start — the one maintained shell ENGINE
//!   the grammar it parses with has, the executing half of the same project, is deliberately
//!   not a dependency of this workspace, so nothing resolves a command name against the
//!   host's `PATH` and nothing is ever handed to the host to run;
//! * it has no network to reach, no HOST filesystem and no host environment: a redirection
//!   writes into the session's own in-memory tree and `cat` reads back out of it, so no host
//!   file is opened, created or read, and the variables it holds are the ones this service
//!   handed it;
//! * it PERFORMS a fixed catalog of session-local utilities and the programs the caller's
//!   own environment declares it provides; every other command, and every command FORM and
//!   expansion it does not perform, fails closed with the status a shell answers for a
//!   command it cannot run — so no caller-chosen program name reaches anything;
//! * every bound it runs under is stated there and none is reachable from a command: the
//!   command budget, the line size, the nesting count, the expansion sizes, the variable
//!   store and the tree. The time a `sleep` states is accounted rather than slept, so a
//!   command holds no thread for it.
//!
//! A sandbox's session reports its two streams SEPARATELY
//! ([`::service_common::posix_shell_session::ShellSession::reporting_both_streams`]),
//! because the execution carries a member for each, and it is given the file utilities over
//! its own working tree, because the environment a sandbox holds has storage of its own.

use std::collections::BTreeMap;

use crate::ops::types::Sandbox;

/// How many lines of what its commands wrote ONE sandbox's session keeps.
///
/// CHOSEN, not an AWS limit: the lines are kept in this process for as long as the store
/// holds the sandbox, so how many one session may keep is bounded here. Past it a line is
/// not kept, and an execution that wrote one reports nothing for it rather than reporting
/// another command's output.
pub const MAX_OUTPUT_LINES: usize = 2_048;

/// How many bytes one kept line carries; a longer line is truncated rather than dropped, so
/// what the command wrote is still reported. CHOSEN for the same reason as
/// [`MAX_OUTPUT_LINES`].
pub const MAX_OUTPUT_LINE_BYTES: usize = 16 * 1024;

/// What ONE command submitted to a sandbox answered: the status the shell gave it, the text
/// it wrote on each stream, and the window it occupied.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    /// The status the shell answered for the command, which is what decides the execution's
    /// status.
    pub exit_code: i32,
    /// The text the command wrote to standard output — `None` for a command that wrote
    /// none, since the member is a non-empty string.
    pub standard_output: Option<String>,
    /// The text the command wrote to standard error, under the same rule.
    pub standard_err: Option<String>,
    /// Epoch millis the command started running at.
    pub started_millis: i64,
    /// Epoch millis it ended at: its start plus the time the command itself stated it took.
    pub ended_millis: i64,
}

/// The session ONE sandbox's commands run in, and the instant that session is free again.
#[derive(Debug)]
pub struct SandboxShell {
    session: ::service_common::posix_shell_session::ShellSession,
    /// Epoch millis the command last run in this session ended at: a sandbox runs one
    /// command at a time, so the next one starts no earlier than this.
    free_at_millis: i64,
}

impl SandboxShell {
    /// A session holding `variables`, with the whole command budget left.
    #[must_use]
    pub fn new(variables: BTreeMap<String, String>) -> Self {
        Self {
            session: ::service_common::posix_shell_session::ShellSession::new(
                variables,
                ::service_common::posix_shell_session::SessionConfig {
                    output: Some(::service_common::posix_shell_session::OutputLimits {
                        lines: MAX_OUTPUT_LINES,
                        line_bytes: MAX_OUTPUT_LINE_BYTES,
                    }),
                    ..::service_common::posix_shell_session::SessionConfig::default()
                },
            )
            // The environment a sandbox holds has a working directory of its own, so the
            // file utilities are commands this session performs over it.
            .with_file_utilities()
            // The execution reports the two streams as members of its own, so a diagnostic
            // is not part of what the command wrote.
            .reporting_both_streams(),
            free_at_millis: 0,
        }
    }

    /// Run `command` in this session, from `from_millis` at the earliest, and answer what it
    /// came to.
    ///
    /// The whole of `command` is handed to the session as ONE program, because that is what
    /// a sandbox is given: `StartCommandExecution$command` is free-form text, so a submission
    /// may be a script whose and-or lists, pipelines and compound commands span lines, and
    /// the grammar the session parses with is what reads them. The status is the last
    /// command's, or the one an `exit` ended the program with — and that `exit` ends this
    /// program and not the sandbox's session, so it is forgotten here
    /// ([`::service_common::posix_shell_session::ShellSession::clear_exit`]) and the next
    /// command submitted to the sandbox runs.
    ///
    /// The session performs the command in process and spends none of the host's clock on it,
    /// so what the command states it took is the time it took — the window the execution
    /// reports.
    pub fn run(&mut self, command: &str, from_millis: i64) -> CommandOutcome {
        let kept_output = self.session.output().len();
        let kept_errors = self.session.errors().len();
        let slept_before = self.session.slept();
        let exit_code = self.session.run(command);
        self.session.clear_exit();
        let slept_millis = i64::try_from(
            self.session
                .slept()
                .saturating_sub(slept_before)
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        let started_millis = from_millis.max(self.free_at_millis);
        let ended_millis = started_millis.saturating_add(slept_millis);
        self.free_at_millis = ended_millis;
        CommandOutcome {
            exit_code,
            standard_output: written_text(&self.session.output()[kept_output..]),
            standard_err: written_text(&self.session.errors()[kept_errors..]),
            started_millis,
            ended_millis,
        }
    }
}

/// The text `lines` come to on a stream: each line as the command wrote it, each ending in
/// the newline that separated it.
///
/// `None` for a command that wrote nothing on that stream, because the members reporting
/// them are non-empty strings (`SensitiveNonEmptyString`, `@length(min: 1)`), so a stream
/// with no text is one the execution states nothing for.
///
/// The session keeps what a command wrote BY THE LINE (a console and a log are read that
/// way), so a command whose last write carried no line ending of its own is reported with
/// one. That is the one difference between this text and the bytes the command wrote.
fn written_text(lines: &[String]) -> Option<String> {
    (!lines.is_empty()).then(|| {
        lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>()
    })
}

/// The variables ONE sandbox's session starts with.
///
/// In the order they resolve, so a later one replaces an earlier one of the same name: the
/// variables CodeBuild itself sets in the environment, read off the sandbox's own record,
/// and then the environment variables that record carries
/// (`ProjectEnvironment$environmentVariables`, "A set of environment variables to make
/// available to builds for this build project").
///
/// A variable whose type is not `PLAINTEXT` names a parameter or a secret another service
/// holds, which nothing resolved for this session, so it sets nothing rather than setting
/// the reference as if it were the value — the rule [`super::buildspec`] states for a
/// build's own session.
pub(super) fn session_variables(sandbox: &Sandbox) -> BTreeMap<String, String> {
    let mut variables = BTreeMap::new();
    // The Region the sandbox RUNS IN, read off its own ARN, which is what its commands are
    // told the Region is — the same pair a build's session carries.
    if let Some(region) = sandbox
        .arn
        .as_deref()
        .and_then(|arn| ::service_common::arn::parse(arn).map(|parsed| parsed.region.to_string()))
        .filter(|region| !region.is_empty())
    {
        variables.insert("AWS_DEFAULT_REGION".to_string(), region.clone());
        variables.insert("AWS_REGION".to_string(), region);
    }
    for variable in sandbox
        .environment
        .as_ref()
        .and_then(|environment| environment.environment_variables.as_deref())
        .unwrap_or_default()
    {
        if variable
            .r#type
            .as_deref()
            .is_none_or(|kind| kind == super::buildspec::PLAINTEXT_VARIABLE_TYPE)
        {
            variables.insert(variable.name.clone(), variable.value.clone());
        }
    }
    variables
}
