// SPDX-License-Identifier: CC0-1.0

//! APPLYING what a deployment's own configuration declares, on the instance the
//! application runs on.
//!
//! What a `.ebextensions/*.config` declares is read where the bundle is read
//! ([`super::ebextensions`]); this is the other half: a deployment WRITES the
//! files it declares to the paths it declares and RUNS the commands it declares
//! on the instance, before the web process is started, so their side effects are
//! there for the application to read. Nothing of that is synthesised: the file is
//! written through the instance's own filesystem and the command is performed by
//! the instance's own shell, both inside the contained execution environment the
//! process runs in — the same engine, the same filesystem and the same bounds as
//! the application's own code, reached from the platform's serving layer
//! ([`super::python_program`], [`super::python_application`]) on the FIRST run of
//! a process, which is when a real deployment applies them.
//!
//! # What "on the instance" reaches, and what it cannot
//!
//! The INSTANCE is the contained execution environment of the embedded
//! interpreter, and nothing here reaches the host this emulator runs on:
//!
//! * the interpreter is built with the native modules that would reach a host
//!   process or a host file left UNREGISTERED and with the operating-system
//!   modules supplied in memory in their place
//!   (`service_com_amazonaws_lambda_awsgirapiservice::state::execution::python`),
//!   so the `open`, `os.makedirs` and `os.chmod` the module below calls write
//!   into the EPHEMERAL STORAGE that environment holds in this process's memory
//!   and nothing else;
//! * the `subprocess` it imports is that runtime's OWN child-process interface
//!   (`…::execution::guest_process`), whose programs are performed by the one
//!   bounded, in-process POSIX shell session this workspace performs
//!   caller-supplied shell programs with (`service_common::posix_shell_session`,
//!   driven the same way by `…::execution::guest_child_process`) over that same
//!   in-memory tree. No host process is started, no host program is resolved and
//!   no host path is opened, whatever a command line says.
//!
//! Beside that, this module CONFINES what it hands over: a declared path is
//! applied only where it resolves under the one directory of that environment
//! anything may write ([`writable_instance_path`]), so a declaration carrying
//! `..`, a relative path or any other root is not applied at all — and the bounds
//! on how many files and commands one deployment declares, and how long each may
//! be, are the reading's ([`super::ebextensions`]).
//!
//! A path the instance has no writable filesystem for, and a command its shell
//! cannot start, leave the account of that in the instance's own log and nothing
//! else: a file nothing wrote is never reported as written.
//!
//! The module below is supplied to the interpreter beside the serving layers, so
//! both apply a deployment's configuration the same way and a bundle carrying a
//! file of that name cannot replace it ([`super::python_modules`]).

use super::*;

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;

/// The module this emulator supplies to apply a deployment's configuration with,
/// and the function in it the serving layers call.
pub const PROVISIONING_MODULE: &str = "__eb_instance";

/// The ONE directory of the instance anything may write: the ephemeral storage the
/// execution environment holds in this process's memory, which is the whole of the
/// writable filesystem the application and the commands run beside it have (the
/// engine's own `/tmp`, every other root of that environment being a read-only
/// mount or absent).
pub const INSTANCE_WRITABLE_ROOT: &str = "/tmp";

/// `path` as a path of the instance this deployment may WRITE — and `None` for a
/// declaration that names anything else.
///
/// A declared path is a caller's: it comes out of the bundle's own configuration
/// file, so what it may name is decided HERE rather than by whatever a path
/// expression would reach. It has to be absolute, carry no `.` or `..` segment
/// and no empty one, and resolve under [`INSTANCE_WRITABLE_ROOT`] — the root
/// itself is not a file to write, and a path outside it is not applied at all.
#[must_use]
pub fn writable_instance_path(path: &str) -> Option<String> {
    let path = path.trim();
    let under = path.strip_prefix(INSTANCE_WRITABLE_ROOT)?.strip_prefix('/')?;
    let segments: Vec<&str> = under.split('/').collect();
    let confined = !segments.is_empty()
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && *segment != "." && *segment != "..");
    confined.then(|| format!("{INSTANCE_WRITABLE_ROOT}/{}", segments.join("/")))
}

/// What the deployment applies on the instance, as the supplied module reads it:
/// the files it writes and the commands it runs, in the order Elastic Beanstalk
/// applies them.
///
/// Only what this instance CAN apply travels: a declared path that is no path of
/// the writable filesystem it has ([`writable_instance_path`]) is left out with
/// the account of it in this emulator's own log, rather than handed over for a
/// write that would reach somewhere a declaration chose.
///
/// Empty for a deployment whose bundle declares nothing to apply, which is what a
/// bundle with no `.ebextensions` leaves.
#[must_use]
pub fn provisioning_document(configuration: &BundleConfiguration) -> ::serde_json::Value {
    ::serde_json::json!({
        "files": configuration
            .files
            .iter()
            .filter_map(|file| {
                let Some(path) = writable_instance_path(&file.path) else {
                    ::tracing::debug!(
                        "a source bundle's configuration declares a file at a path this \
                         instance has no writable filesystem for, so the deployment writes \
                         none",
                    );
                    return None;
                };
                Some(::serde_json::json!({
                    "path": path,
                    "content": file.content,
                    "mode": file.mode,
                }))
            })
            .collect::<Vec<::serde_json::Value>>(),
        "commands": declared(&configuration.commands),
        "container_commands": declared(&configuration.container_commands),
    })
}

/// The commands as that module reads them.
///
/// The directory a command states is confined exactly as a declared file's path is
/// ([`writable_instance_path`]); one that names anything else is left out, and the
/// command runs where the deployment itself is.
fn declared(commands: &[DeclaredCommand]) -> Vec<::serde_json::Value> {
    commands
        .iter()
        .map(|command| {
            ::serde_json::json!({
                "name": command.name,
                "command": command.command,
                "cwd": command
                    .cwd
                    .as_deref()
                    .and_then(writable_instance_path),
            })
        })
        .collect()
}

/// The supplied module itself, as the interpreter is handed it.
#[must_use]
pub fn provisioning_module() -> PackageModule {
    PackageModule {
        path: format!("{PROVISIONING_MODULE}{PYTHON_SUFFIX}"),
        source: PROVISIONING_SOURCE.to_string(),
    }
}

/// The module this emulator hands the interpreter to APPLY a deployment's
/// configuration on the instance: the `files:` it declares and the `commands:`
/// and `container_commands:` it runs.
///
/// Written over the instance's own filesystem and its own shell — `open`,
/// `os.makedirs`, `os.chmod` and the child-process interface of the standard
/// library THIS RUNTIME supplies, none of which reaches the host (see this
/// module's header) — so what a declared file holds afterwards and what a
/// declared command left behind are what really happened on this instance.
///
/// It confines what it applies a second time, inside the environment: a path it
/// writes has to resolve under the one writable directory that environment has,
/// which is also what the host side hands it ([`writable_instance_path`]).
const PROVISIONING_SOURCE: &str = r#"
"""Applying what a deployment's own `.ebextensions` declare, on this instance."""

import sys

# The ONE directory of this instance anything may write: the ephemeral storage this execution
# environment holds in memory. Every other root of it is a read-only mount or absent, and the
# host side hands over nothing outside it either.
_WRITABLE_ROOT = "/tmp"

# The program the instance's shell is: the child-process interface of this runtime performs it
# with the one bounded, in-process shell session this emulator performs shell programs with, so
# a command line is read by that session's own grammar and reaches no host program.
_SHELL = ["/bin/sh", "-c"]

# Everything else this module needs it imports where it uses it, so a process that applies
# nothing imports nothing for it.


def _confined(path):
    """`path` as a path of this instance's writable storage, or `None` for anything else.

    The declaration is a caller's, so the path it names is resolved — which is what collapses
    a `..` or a symlinked parent — and taken only where it stays under that root.
    """
    import os

    if not isinstance(path, str) or not path:
        return None
    resolved = os.path.realpath(path)
    root = _WRITABLE_ROOT.rstrip("/")
    if resolved != root and not resolved.startswith(root + "/"):
        return None
    return resolved


def _written(entry):
    """Write ONE file the deployment's configuration declares, at the path it declares."""
    import os

    path = _confined(entry.get("path"))
    if path is None:
        print(
            "eb: %s is no path of this instance's writable storage, so nothing was written"
            % (entry.get("path"),),
            flush=True,
        )
        return
    directory = os.path.dirname(path)
    if directory:
        try:
            os.makedirs(directory, exist_ok=True)
        except OSError:
            pass
    try:
        with open(path, "w") as writing:
            writing.write(str(entry.get("content") or ""))
    except OSError as failure:
        # A path this instance has no writable filesystem for: the account of it goes to the
        # instance's own log, and nothing reports a file that was not written as written.
        print("eb: %s was not written: %s" % (path, failure), flush=True)
        return
    mode = entry.get("mode")
    if mode:
        try:
            os.chmod(path, int(str(mode)[-4:], 8))
        except (OSError, ValueError, TypeError):
            pass


def _performed(entry):
    """RUN one command the deployment's configuration declares, through this instance's shell.

    The command line is handed over as the ARGUMENT of that shell rather than as a program
    name, which is what a deployment's own command is: one line for the instance's shell to
    read. Its output goes to the instance's own log, which is where the output of a
    deployment's commands belongs; a command that could not be started at all is reported
    there too.
    """
    import os
    import subprocess

    line = str(entry.get("command") or "")
    if not line.strip():
        return
    cwd = _confined(entry.get("cwd"))
    if cwd:
        try:
            os.makedirs(cwd, exist_ok=True)
        except OSError:
            # A directory this instance cannot hold is no directory to run in: the command
            # runs where the deployment itself is instead of not running at all.
            cwd = None
    try:
        finished = subprocess.run(
            _SHELL + [line],
            shell=False,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
    except OSError as failure:
        print("eb: %s did not run: %s" % (line, failure), flush=True)
        return
    written = finished.stdout or b""
    if isinstance(written, bytes):
        written = written.decode("utf-8", "replace")
    if written:
        sys.stdout.write(written)
    if finished.returncode:
        print("eb: %s exited %s" % (line, finished.returncode), flush=True)


def provisioned(provisioning):
    """Apply what the deployment declares, in the order Elastic Beanstalk applies it.

    The files it writes, the commands that run before the application is set up, and then the
    container commands that run once it has been extracted and before the web process starts.
    """
    if not provisioning:
        return
    for entry in provisioning.get("files") or []:
        _written(entry)
    for entry in provisioning.get("commands") or []:
        _performed(entry)
    for entry in provisioning.get("container_commands") or []:
        _performed(entry)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a declaration is applied only where it names a path of the ONE
    /// writable directory the instance has — so a relative path, a path of any
    /// other root, the root itself and a path climbing out of it are applied
    /// nowhere.
    #[test]
    fn only_a_path_of_the_instances_writable_storage_is_applied() {
        // The rows span what the reading generalises over: a path under the root,
        // one nested deeper, the spellings that reach back out of it (`..`, a
        // doubled separator, a `.` segment), the root itself, another root of the
        // instance, a relative path, and a path that only starts like the root.
        let cases: &[(&str, Option<&str>)] = &[
            ("/tmp/written.txt", Some("/tmp/written.txt")),
            ("/tmp/eb/nested/written.txt", Some("/tmp/eb/nested/written.txt")),
            ("  /tmp/written.txt  ", Some("/tmp/written.txt")),
            ("/tmp/../etc/passwd", None),
            ("/tmp/./written.txt", None),
            ("/tmp//written.txt", None),
            ("/tmp", None),
            ("/tmp/", None),
            ("/etc/passwd", None),
            ("/var/app/current/written.txt", None),
            ("written.txt", None),
            ("", None),
            ("/tmpfile/written.txt", None),
        ];
        for (path, expected) in cases {
            let confined = writable_instance_path(path);
            assert_eq!(
                confined.as_deref(),
                *expected,
                "case {path:?}: expected {expected:?}, got {confined:?}"
            );
        }
    }

    /// RULE: only what the instance can apply travels to it — a file at a path
    /// outside its writable storage is in no document, and a command keeps its
    /// own line while a directory outside that storage is dropped from it.
    #[test]
    fn the_document_carries_only_what_the_instance_can_apply() {
        let configuration = BundleConfiguration {
            option_settings: Vec::new(),
            files: vec![
                DeclaredFile {
                    path: "/tmp/kept.txt".to_string(),
                    content: "kept".to_string(),
                    mode: Some("000644".to_string()),
                },
                DeclaredFile {
                    path: "/etc/passwd".to_string(),
                    content: "not applied".to_string(),
                    mode: None,
                },
            ],
            commands: Vec::new(),
            container_commands: vec![DeclaredCommand {
                name: "01_write".to_string(),
                command: "printf token > /tmp/written".to_string(),
                cwd: Some("/var/app/staging".to_string()),
            }],
        };
        let document = provisioning_document(&configuration);
        let files = document["files"].as_array().expect("the files travel");
        let paths: Vec<&str> = files
            .iter()
            .filter_map(|file| file["path"].as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["/tmp/kept.txt"],
            "expected only the file the instance can write, got {paths:?}"
        );
        let commands = document["container_commands"]
            .as_array()
            .expect("the commands travel");
        assert_eq!(
            commands.len(),
            1,
            "expected the command to travel, got {commands:?}"
        );
        assert_eq!(
            commands[0]["command"].as_str(),
            Some("printf token > /tmp/written"),
            "expected the command line the configuration declared, got {commands:?}"
        );
        assert!(
            commands[0]["cwd"].is_null(),
            "expected a directory outside the writable storage to be dropped, got {commands:?}"
        );
    }
}
