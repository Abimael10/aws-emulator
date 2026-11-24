// SPDX-License-Identifier: CC0-1.0

//! What the BUILD of a MicroVM image version makes of the code artifact it fetched: the
//! build steps the artifact's own container specification declares, PERFORMED, and the
//! verdict that gives the version.
//!
//! `CreateMicrovmImage` documents the build as asynchronous — "the image transitions from
//! CREATING to CREATED on success, or CREATE_FAILED on failure" — and what decides the
//! branch is the code artifact itself: a build context whose own instructions do not
//! complete produces no image, however well the artifact was served
//! ([`super::code_artifact`] decides only whether it was served at all). The `Dockerfile`
//! of the context is where those instructions live, and a `RUN` is "a command in a new
//! layer": the builder performs it in a shell and STOPS at the first one that ends on a
//! failure, which is the whole of this module's rule.
//!
//! So the steps are PERFORMED, never inspected: each one runs in the bounded, in-process
//! POSIX session this workspace performs caller-supplied shell programs in
//! (`::service_common::posix_shell_session`, whose module docs state the catalog it
//! performs), over ONE tree seeded with the artifact's own files — so a step reads what
//! the artifact holds and what an earlier step wrote. Nothing starts a process, opens a
//! host path or reaches a socket.
//!
//! A step the session has no program for establishes NOTHING: the emulator holds no such
//! utility, which is a fact about the emulator and not about the artifact, so such a step
//! is passed over exactly as an unanswered artifact read is
//! ([`super::code_artifact::CodeArtifactVerdict::Unestablished`]). A build is refuted only
//! by a step that RAN and reported a failure.

use std::collections::BTreeMap;

use ::service_common::posix_shell_session::{
    Bounds, OutputLimits, SessionConfig, ShellSession, Statuses,
};

/// The file a container build context declares its build steps in.
const CONTAINER_SPEC: &str = "Dockerfile";

/// The instruction that RUNS a command as part of the build.
const RUN_INSTRUCTION: &str = "RUN";

/// How many build steps of one artifact are performed.
///
/// The instructions are the caller's own text, so what one build can cost is bounded —
/// well above an ordinary image's step count, and far below what would cost this process
/// anything noticeable. The steps past the budget are not performed, and a build is never
/// refuted by a step that did not run.
pub const MAX_BUILD_STEPS: usize = 64;

/// How many command lines ONE build step performs: a step is a single command line, and
/// the `&&` list a step is commonly written as is one line.
const MAX_STEP_COMMANDS: usize = 32;

/// How many lines of what a build step wrote are kept while it runs. The build reports the
/// step and its status rather than its log, so this is only what the session holds.
const MAX_STEP_OUTPUT_LINES: usize = 64;

/// How many bytes of one of those lines are kept.
const MAX_STEP_OUTPUT_LINE_BYTES: usize = 4 * 1024;

/// The `stateReason` a build settles with when one of the artifact's own build steps ended
/// on a failure: the sentence a container builder reports it with, carrying the command
/// that failed and the status it ended on, both read off the artifact and the run.
#[must_use]
pub fn step_failure_reason(command: &str, status: i32) -> String {
    format!("The command '/bin/sh -c {command}' returned a non-zero code: {status}")
}

/// The session one build's steps are performed in: the whole command catalog over a tree
/// of the build's own, and this build's budget.
fn build_config() -> SessionConfig {
    SessionConfig {
        bounds: Bounds {
            commands: MAX_BUILD_STEPS.saturating_mul(MAX_STEP_COMMANDS),
            ..Bounds::default()
        },
        output: Some(OutputLimits {
            lines: MAX_STEP_OUTPUT_LINES,
            line_bytes: MAX_STEP_OUTPUT_LINE_BYTES,
        }),
        ..SessionConfig::default()
    }
}

/// What performing one artifact's own build steps established: the reason the build
/// settles on the failure branch, and the files the steps LEFT in the image.
///
/// A `RUN` is "a command in a new layer", so what it writes is part of the image the build
/// produces: a context that materialises its guest with `RUN` steps (an interpreter's
/// source file written out by one of them) declares its program there rather than as an
/// entry of the archive, and the files below are that program
/// ([`super::guest_program::read`] reads them beside the archive's own entries).
#[derive(Debug, Default, Clone)]
pub struct Built {
    /// The `stateReason` the version and its builds settle with, and `None` for a build
    /// this emulator could not refute.
    pub failure_reason: Option<String>,
    /// The files the performed steps left, keyed by the absolute path each was written at.
    pub files: Vec<(String, String)>,
}

/// PERFORM the build steps the artifact `bytes` declares, over one tree seeded with the
/// artifact's own files, and report what that established.
///
/// `key` is the object key the artifact was fetched from, which names the single entry of
/// an artifact that is not an archive.
#[must_use]
pub fn perform(bytes: &[u8], key: &str) -> Built {
    let entries = super::guest_program::artifact_entries(bytes, key);
    let Some(spec) = entries
        .iter()
        .find(|entry| entry.path == CONTAINER_SPEC)
        .map(|entry| entry.source.clone())
    else {
        return Built::default();
    };
    let steps = build_steps(&spec);
    // The tree the steps run over is the build context: the artifact's own files, so a
    // step reads what the artifact holds rather than an empty directory. It is the IMAGE's
    // own storage, bounded by what a MicroVM's machine holds
    // ([`super::guest_machine::volume_limits`]), and it is taken back below, so what the
    // steps wrote is part of the image they built.
    let mut tree = ::service_common::posix_shell_volume::ShellVolume::new(
        super::guest_machine::volume_limits(),
    );
    for entry in &entries {
        // The context is the directory the steps run in, which is the root of the tree:
        // a step naming `Dockerfile` reads the artifact's own entry.
        tree.put_file(&format!("/{}", entry.path), &entry.source);
    }
    let mut session = ShellSession::in_volume(BTreeMap::new(), build_config(), tree)
        .with_file_utilities();
    let statuses = Statuses::default();
    let mut failure_reason = None;
    for step in steps {
        let status = session.run(&step);
        // Each step is its own shell, as each layer of a container build is, so a step
        // that ended itself ends that step and not the build.
        let ended = session.exit_status();
        session.clear_exit();
        let status = ended.unwrap_or(status);
        if status == statuses.success || status == statuses.could_not_run {
            continue;
        }
        failure_reason = Some(step_failure_reason(&step, status));
        break;
    }
    let files = session
        .into_volume()
        .map(|tree| {
            tree.stored_files()
                .map(|(path, contents)| (path.to_string(), contents.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Built {
        failure_reason,
        files,
    }
}

/// The reason the build of the artifact `bytes` settles on the FAILURE branch, and `None`
/// for an artifact whose own build steps this emulator could not refute.
///
/// `key` is the object key the artifact was fetched from, which names the single entry of
/// an artifact that is not an archive.
#[must_use]
pub fn refuted(bytes: &[u8], key: &str) -> Option<String> {
    perform(bytes, key).failure_reason
}

/// The build steps `dockerfile` declares, in the order the builder performs them: the
/// command of every `RUN` instruction it carries.
///
/// An instruction continued over several lines with a trailing `\` is ONE step, as the
/// builder reads it, and a comment line is none. Both forms a `RUN` is written in are
/// read: the shell form (`RUN make build`) and the JSON array form (`RUN ["make",
/// "build"]`), whose words make up the one command line.
fn build_steps(dockerfile: &str) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();
    let mut joined = String::new();
    for line in dockerfile.lines() {
        let trimmed = line.trim();
        if joined.is_empty() && (trimmed.is_empty() || trimmed.starts_with('#')) {
            continue;
        }
        match trimmed.strip_suffix('\\') {
            Some(head) => {
                joined.push_str(head.trim_end());
                joined.push(' ');
                continue;
            }
            None => {
                joined.push_str(trimmed);
            }
        }
        let instruction = std::mem::take(&mut joined);
        if let Some(step) = run_command(&instruction) {
            steps.push(step);
            if steps.len() >= MAX_BUILD_STEPS {
                break;
            }
        }
    }
    // An instruction left open by a trailing `\` at the end of the file is still the
    // instruction the context declared.
    if let Some(step) = run_command(joined.trim()) {
        if steps.len() < MAX_BUILD_STEPS {
            steps.push(step);
        }
    }
    steps
}

/// The command one instruction runs, or `None` for an instruction that is not a `RUN`.
fn run_command(instruction: &str) -> Option<String> {
    let (name, arguments) = instruction.split_once(char::is_whitespace)?;
    if !name.eq_ignore_ascii_case(RUN_INSTRUCTION) {
        return None;
    }
    let arguments = arguments.trim();
    let command = match exec_form_words(arguments) {
        Some(words) => words.join(" "),
        None => arguments.to_string(),
    };
    (!command.is_empty()).then_some(command)
}

/// The words of a `RUN` written in JSON array form (`["make", "build"]`), and `None` for
/// one written in shell form.
fn exec_form_words(arguments: &str) -> Option<Vec<String>> {
    let parsed: ::serde_json::Value = ::serde_json::from_str(arguments).ok()?;
    let words = parsed.as_array()?;
    words
        .iter()
        .map(|word| word.as_str().map(::std::string::ToString::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the build steps of a context are the commands of its `RUN` instructions, in
    /// order, whichever form each is written in.
    #[test]
    fn the_steps_are_the_run_instructions_the_context_declares() {
        // (case, the Dockerfile, the steps the builder performs)
        let rows: Vec<(&str, &str, Vec<&str>)> = vec![
            (
                "no step at all",
                "FROM node:24-alpine\nCMD [\"node\", \"server.js\"]\n",
                vec![],
            ),
            (
                "a shell-form step",
                "FROM node:24-alpine\nRUN exit 1\nCMD [\"node\", \"server.js\"]\n",
                vec!["exit 1"],
            ),
            (
                "a JSON array step",
                "FROM scratch\nRUN [\"touch\", \"/built\"]\n",
                vec!["touch /built"],
            ),
            (
                "a step continued over lines",
                "FROM scratch\nRUN echo one \\\n  && echo two\n",
                vec!["echo one && echo two"],
            ),
            (
                "comments and blank lines are no steps",
                "# a comment\n\nFROM scratch\n# RUN not-a-step\nRUN true\n",
                vec!["true"],
            ),
            (
                "a lowercase instruction",
                "from scratch\nrun false\n",
                vec!["false"],
            ),
            (
                "several steps keep their order",
                "FROM scratch\nRUN echo one\nRUN echo two\n",
                vec!["echo one", "echo two"],
            ),
        ];
        for (case, dockerfile, expected) in rows {
            let actual = build_steps(dockerfile);
            let expected: Vec<String> = expected
                .into_iter()
                .map(::std::string::ToString::to_string)
                .collect();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the steps {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a build is refuted by a step that RAN and reported a failure, and by nothing
    /// else — not by a context with no steps, not by steps that all succeed, and not by a
    /// step this emulator has no program for.
    #[test]
    fn only_a_step_that_ran_and_failed_refutes_a_build() {
        // (case, the Dockerfile, whether the build is refuted)
        let rows: Vec<(&str, &str, bool)> = vec![
            ("no steps", "FROM scratch\nCMD [\"node\", \"app.js\"]\n", false),
            ("a step that succeeds", "FROM scratch\nRUN true\n", false),
            ("a step that fails", "FROM scratch\nRUN exit 1\n", true),
            ("a step that reports failure", "FROM scratch\nRUN false\n", true),
            (
                "a step whose and-list fails",
                "FROM scratch\nRUN echo one && exit 3\n",
                true,
            ),
            (
                "a utility this emulator does not hold",
                "FROM scratch\nRUN npm ci --omit=dev\n",
                false,
            ),
            (
                "a later step fails after an earlier one succeeded",
                "FROM scratch\nRUN echo one > /log.txt\nRUN exit 2\n",
                true,
            ),
            (
                "a step reads what the artifact holds",
                "FROM scratch\nRUN test -f Dockerfile\n",
                false,
            ),
        ];
        for (case, dockerfile, expected) in rows {
            let artifact = dockerfile.as_bytes();
            let actual = refuted(artifact, CONTAINER_SPEC);
            assert_eq!(
                actual.is_some(),
                expected,
                "case {case:?}: expected refuted={expected}, got {actual:?}"
            );
        }
    }

    /// RULE: an artifact holding no container specification declares no build step, so
    /// nothing about its build is established here.
    #[test]
    fn an_artifact_with_no_specification_refutes_nothing() {
        let actual = refuted(b"// the guest", "guest.js");
        assert!(
            actual.is_none(),
            "expected no verdict for an artifact with no specification, got {actual:?}"
        );
    }

    /// RULE: what a build's own `RUN` steps write is part of the image they built, so the
    /// files the steps left are reported with the verdict.
    #[test]
    fn the_files_the_steps_wrote_are_part_of_the_image() {
        let dockerfile =
            "FROM scratch\nRUN mkdir -p /srv\nRUN echo baked > /srv/server.js\n";
        let built = perform(dockerfile.as_bytes(), CONTAINER_SPEC);
        assert!(
            built.failure_reason.is_none(),
            "expected a build nothing refuted, got {built:?}"
        );
        let found = built
            .files
            .iter()
            .find(|(path, _)| path.ends_with("server.js"));
        assert_eq!(
            found.map(|(path, source)| (path.as_str(), source.as_str())),
            Some(("/srv/server.js", "baked\n")),
            "expected the file the step wrote, got {:?}",
            built.files
        );
    }

    /// RULE: the reason a refuted build settles with names the command that failed and the
    /// status it ended on.
    #[test]
    fn the_reason_names_the_step_and_its_status() {
        let reason = refuted(b"FROM scratch\nRUN exit 7\n", CONTAINER_SPEC)
            .expect("a step that ran and failed refutes the build");
        assert_eq!(
            reason,
            step_failure_reason("exit 7", 7),
            "expected the step and its status in the reason, got {reason:?}"
        );
    }
}
