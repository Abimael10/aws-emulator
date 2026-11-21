// SPDX-License-Identifier: CC0-1.0

//! The `Procfile` a source bundle carries: the command the platform starts the
//! application's web process with.
//!
//! Elastic Beanstalk's own documentation for the current platform generations: a
//! source bundle may carry a file named `Procfile` at its ROOT, one
//! `<process name>: <command>` line per process, and the platform runs the
//! command of the `web` process as the application that answers the environment's
//! HTTP traffic — "if you include a Procfile, Elastic Beanstalk uses the command
//! it states instead of the platform's default start command". A bundle carrying
//! none leaves the platform starting the application the way its own default
//! command does (for a Python platform, the WSGI server on the file `WSGIPath`
//! names — see [`super::python_application`]).
//!
//! What this module owns is only READING that file: which command one process is
//! given. What that command then starts, and in which engine, is decided where a
//! deployment's entry point is ([`super::deployed_application::application_entry`]).
//!
//! Split out of [`super::deployed_application`], which owns the files a bundle
//! carries.

use super::*;

/// The file a source bundle states its process commands in, at the bundle's root.
pub const PROCFILE: &str = "Procfile";

/// The process whose command serves the environment's own HTTP traffic: "the web
/// process is the one that handles HTTP requests", and the only one the proxy on
/// the instance forwards a request to.
pub const WEB_PROCESS: &str = "web";

/// What starts a COMMENT line of the file, which names no process.
const COMMENT: char = '#';

/// The command `bundle`'s `Procfile` gives the process `process`, or `None` when
/// the bundle carries no such file, states no line for that process, or states it
/// with no command at all.
///
/// The first line naming the process is the one that counts: a file stating a
/// process twice gives the platform one command for it, and that is the one it
/// read first.
#[must_use]
pub fn procfile_command(bundle: &ApplicationBundle, process: &str) -> Option<String> {
    let procfile = bundle.source(PROCFILE)?;
    procfile
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(COMMENT))
        .filter_map(|line| line.split_once(':'))
        .find(|(named, _)| named.trim() == process)
        .map(|(_, command)| command.trim().to_string())
        .filter(|command| !command.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle carrying `files`, read the way a source bundle is.
    fn bundle(files: &[(&str, &[u8])]) -> ApplicationBundle {
        ApplicationBundle {
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_string(), bytes.to_vec()))
                .collect(),
        }
    }

    /// One `Procfile` and the command it gives a process: a row of
    /// [`the_command_is_the_one_the_named_process_line_states`].
    struct CommandRow {
        case: &'static str,
        /// The files the bundle carries.
        files: &'static [(&'static str, &'static [u8])],
        /// The process the command is read for.
        process: &'static str,
        /// The command that process is given.
        command: Option<&'static str>,
    }

    /// RULE: a process's command is what its own `<name>: <command>` line of the
    /// bundle's `Procfile` states — the first such line, with the surrounding
    /// space dropped — and a bundle with no such file, no line for the process, or
    /// no command on it gives that process none.
    #[test]
    fn the_command_is_the_one_the_named_process_line_states() {
        let cases: &[CommandRow] = &[
            CommandRow {
                case: "the web process of a file stating one process",
                files: &[("Procfile", b"web: python3 -u server.py\n")],
                process: WEB_PROCESS,
                command: Some("python3 -u server.py"),
            },
            CommandRow {
                case: "a process named beside the web one",
                files: &[("Procfile", b"web: node app.js\nworker: node queue.js\n")],
                process: "worker",
                command: Some("node queue.js"),
            },
            CommandRow {
                case: "a command whose own arguments carry a colon",
                files: &[("Procfile", b"web: gunicorn --bind :8000 project.wsgi:application")],
                process: WEB_PROCESS,
                command: Some("gunicorn --bind :8000 project.wsgi:application"),
            },
            CommandRow {
                case: "a file whose lines carry comments and blank space",
                files: &[("Procfile", b"\n# the web process\n  web :   python3 server.py  \n")],
                process: WEB_PROCESS,
                command: Some("python3 server.py"),
            },
            CommandRow {
                case: "a file stating the process twice",
                files: &[("Procfile", b"web: python3 first.py\nweb: python3 second.py\n")],
                process: WEB_PROCESS,
                command: Some("python3 first.py"),
            },
            CommandRow {
                case: "a file stating no line for the process",
                files: &[("Procfile", b"worker: node queue.js\n")],
                process: WEB_PROCESS,
                command: None,
            },
            CommandRow {
                case: "a line stating no command at all",
                files: &[("Procfile", b"web:   \n")],
                process: WEB_PROCESS,
                command: None,
            },
            CommandRow {
                case: "a bundle carrying no such file",
                files: &[("application.py", b"application = None\n")],
                process: WEB_PROCESS,
                command: None,
            },
            CommandRow {
                case: "a file whose bytes are not text",
                files: &[("Procfile", &[0xff, 0xfe, 0x00])],
                process: WEB_PROCESS,
                command: None,
            },
        ];
        for row in cases {
            let case = row.case;
            let actual = procfile_command(&bundle(row.files), row.process);
            let expected = row.command.map(str::to_string);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
