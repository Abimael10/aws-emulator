// SPDX-License-Identifier: CC0-1.0

//! The buildspec: the phases a build runs, and what running them answered.
//!
//! # The rule this module implements
//!
//! A build RUNS the commands its buildspec declares, phase by phase, and its terminal
//! `buildStatus` is what those commands answered: `SUCCEEDED` when every command of
//! every phase exited zero, and `FAILED` when one did not. `FAULT` is CodeBuild's
//! build-SYSTEM failure, so it is not what a build whose own commands failed reports.
//! Each phase's `phaseStatus` is the same answer for that phase's commands, and the failed
//! phase keeps its own `FAILED` status however the build goes on. WHAT a failure does to
//! the phases after it is decided by `on-failure` and by WHERE in the sequence the failing
//! phase sits: `CONTINUE` goes on to the next phase, however the failure came about and
//! however many phases have already failed, while `ABORT` — the default — ends the build at
//! a phase that PREPARES it ([`PREPARING_PHASES`]), so the phases after that one never ran
//! and the build reports none of them. From the BUILD phase on, a failure stops none of the
//! work CodeBuild does after that phase: the build's commands have run and what they wrote
//! is in its working tree, so the later command phases still run and the build still
//! packages what it produced. A phase's `finally` block runs after its commands whatever
//! they answered, and an `exit` ends the command BLOCK it was performed in — CodeBuild runs
//! each block in its own shell instance — so it ends neither the `finally` block after it
//! nor the phases after that.
//!
//! A build that ran its commands then does its own finishing work whether they succeeded
//! or failed: what it produced is put where its artifacts say and the build reports an
//! `UPLOAD_ARTIFACTS` phase for it, so a build its own build phase FAILED uploads what it
//! did produce — a `finally` block's output among it — exactly as a successful one does.
//! That transfer is the object store's half of the run and is
//! performed by [`super::build_run`], which calls [`run_commands`] and then
//! [`finish_build`] with what the transfer answered: this module decides nothing about
//! where an artifact went. Nor does it decide what the build's `reports` sections came to:
//! this reading only states which files each of them declared and in what format, and
//! [`super::build_reports`] reads the runs out of those files.
//!
//! Each phase a build reports occupies the time ITS OWN commands took, and the build's
//! window is the one every phase window lies inside: that is the clock [`BuildClock`]
//! keeps, and it is also what ends a build whose execution timeout fell due while a phase
//! was still running — the phase that was running then is the one that `TIMED_OUT`.
//!
//! The buildspec is YAML, read through the one bounded parse this crate reads a
//! buildspec with ([`super::batchspec::buildspec_document`]), and its command lines are
//! run by the build environment's shell ([`super::build_shell`]) — which performs, beside
//! its own utilities, the programs of the runtimes the install phase's `runtime-versions`
//! selected ([`super::runtime_versions`]). That environment is the build's EFFECTIVE one:
//! the commands run inside the image it names, so the variable naming that image and the
//! files the image presents are what they see of it ([`super::build_image`]). A build whose buildspec cannot be read has no
//! phase to run: the source-download phase fails, and what it reports is what the reading
//! answered — the YAML parser's own diagnostic for a buildspec that is not YAML, and
//! CodeBuild's `YAML_FILE_ERROR` classification for a buildspec there was none of to parse.
//! A build whose runtime selection the environment cannot satisfy has none either, and that
//! same phase reports what CodeBuild states about the selection
//! ([`runtime_install_context`]) — as does a build whose own SOURCE could not be obtained
//! ([`BuildSeed::source_failure`]).

use std::collections::BTreeMap;

use super::batchspec::{buildspec_document, mapping_get, scalar_string, UnreadableBuildspec};
use super::build_image::{image_files, BUILD_IMAGE_VARIABLE};
use super::build_reports::ReportDeclaration;
use super::build_shell::BuildShell;
use super::runtime_versions::{install_runtimes, RuntimeInstallFailure};
use super::{finished_phase, open_phase};
use crate::ops::types::{Build, BuildPhase, ExportedEnvironmentVariable, PhaseContext};

/// `StatusType` values a build and its phases report.
pub const BUILD_STATUS_SUCCEEDED: &str = "SUCCEEDED";
/// See [`BUILD_STATUS_SUCCEEDED`].
pub const BUILD_STATUS_FAILED: &str = "FAILED";

/// The `StatusType` the phase a build's EXECUTION timeout ended reports
/// (`smithy_query(query="shape",shape="StatusType")`: SUCCEEDED, FAILED, FAULT, TIMED_OUT,
/// IN_PROGRESS, STOPPED, documented on `BuildPhase$phaseStatus` as "the build phase timed
/// out"). The build itself is `FAILED`: the scalar `TIMED_OUT` is the outcome of a build
/// that never left the queue at all, whose own wait is `queuedTimeoutInMinutes`.
const PHASE_STATUS_TIMED_OUT: &str = "TIMED_OUT";

/// `BuildPhaseType` values, in the order a build reaches them.
const PHASE_SUBMITTED: &str = "SUBMITTED";
/// See [`PHASE_SUBMITTED`].
const PHASE_QUEUED: &str = "QUEUED";
/// See [`PHASE_SUBMITTED`].
const PHASE_PROVISIONING: &str = "PROVISIONING";
/// See [`PHASE_SUBMITTED`].
const PHASE_DOWNLOAD_SOURCE: &str = "DOWNLOAD_SOURCE";
/// See [`PHASE_SUBMITTED`].
const PHASE_UPLOAD_ARTIFACTS: &str = "UPLOAD_ARTIFACTS";
/// See [`PHASE_SUBMITTED`].
const PHASE_FINALIZING: &str = "FINALIZING";
/// See [`PHASE_SUBMITTED`].
const PHASE_COMPLETED: &str = "COMPLETED";

/// The phases a build RUNS the commands of, as the `BuildPhaseType` each is reported by
/// paired with the buildspec key that declares it, in the order CodeBuild runs them.
///
/// From the buildspec reference's `phases` sequence: `install`, `pre_build`, `build` and
/// `post_build` are the phases whose `commands` the build runs. Every build runs ALL FOUR
/// and reports every one it reached, whether or not the buildspec declares that key: a
/// phase that declares nothing runs no command, so it succeeds — it is still a phase the
/// build went through.
const COMMAND_PHASES: [(&str, &str); 4] = [
    ("install", "INSTALL"),
    ("pre_build", "PRE_BUILD"),
    ("build", "BUILD"),
    ("post_build", "POST_BUILD"),
];

/// The command phases that run BEFORE the build phase, as the `BuildPhaseType` each is
/// reported by: the ones whose failure under `on-failure: ABORT` ENDS the build.
///
/// `on-failure: ABORT` "aborts the build", and what a build has left to do depends on where
/// it is: `install` and `pre_build` only PREPARE it, so a build that failed to install what
/// it needs or to get ready has nothing left to run and nothing it produced, and CodeBuild
/// stops there. Once the build phase has run, the commands have produced whatever they
/// produced, and the work CodeBuild does after that phase still happens: `post_build` runs
/// after a failing build phase — which is what the build environment's documented
/// `CODEBUILD_BUILD_SUCCEEDING` ("whether the current build is succeeding") is there for a
/// command of that phase to read — and the build's own artifact upload packages what the
/// commands wrote.
const PREPARING_PHASES: [&str; 2] = ["INSTALL", "PRE_BUILD"];

/// Whether a FAILING phase reported as `phase_type` ends the build under `on-failure:
/// ABORT` ([`PREPARING_PHASES`]).
fn failure_ends_the_build(phase_type: &str) -> bool {
    PREPARING_PHASES.contains(&phase_type)
}

/// The `PhaseContext$statusCode` CodeBuild reports when the buildspec itself could not
/// be read, and the two readings that carry it.
const YAML_FILE_ERROR: &str = "YAML_FILE_ERROR";

/// What CodeBuild states when the build has no buildspec to run at all: the project
/// carries none inline, so the specification would have to come from a source tree, and
/// this build environment fetches none.
const NO_YAML_FILE_MESSAGE: &str = "YAML file does not exist";

/// What CodeBuild states when the buildspec is there, parses, and is still not the
/// document a build runs — it holds no mapping, or it is outside the bounds this crate
/// reads a buildspec in. A document the PARSER refused is not this: the parser's own
/// diagnostic is what the phase reports for that, unclassified.
const UNREADABLE_YAML_MESSAGE: &str = "Unable to parse YAML file";

/// The `PhaseContext$statusCode` a phase whose command failed reports.
const COMMAND_EXECUTION_ERROR: &str = "COMMAND_EXECUTION_ERROR";

/// The `contexts` a phase that reached its end with NOTHING to report carries.
///
/// A build's phases each carry a `contexts` list "especially to help troubleshoot a failed
/// build", and the list is there whether or not the phase had anything to put in it: a
/// phase with nothing to report carries one context whose `statusCode` and `message` are
/// both empty, so a client walking `contexts` reads an empty report rather than an absent
/// one. The two phases before the environment exists — the build having been submitted and
/// its wait in the queue — are not phases of the build environment and carry none
/// ([`leading_phases`]), and neither does the terminal marker phase, which has not ended.
fn empty_contexts() -> Option<Vec<PhaseContext>> {
    Some(vec![PhaseContext {
        status_code: Some(String::new()),
        message: Some(String::new()),
    }])
}

/// `contexts` for a phase that ended: what it reported, or the empty report of a phase
/// that had nothing to report ([`empty_contexts`]).
fn phase_contexts(reported: Option<PhaseContext>) -> Option<Vec<PhaseContext>> {
    match reported {
        Some(context) => Some(vec![context]),
        None => empty_contexts(),
    }
}

/// What CodeBuild states as the reason a phase's command failed: the command it ran and
/// the status that command exited with.
fn command_failure_message(command: &str, status: i32) -> String {
    format!("Error while executing command: {command}. Reason: exit status {status}")
}

/// What the phase that was RUNNING when the build's execution timeout fell due reports: the
/// build did not get marked as completed inside the wait it was given, so CodeBuild ended it
/// there.
///
/// `minutes` is the wait that build was given, which is the one variable part of it
/// (`Build$timeoutInMinutes`, "How long, in minutes, from 5 to 2160 (36 hours), for
/// CodeBuild to wait before timing out this build if it does not get marked as completed").
/// AWS publishes no sentence for the condition, so this states it in this service's own
/// terms, and classifies it with no status code: the phase's own `TIMED_OUT` status is what
/// says which condition it was.
fn timed_out_context(minutes: i32) -> PhaseContext {
    PhaseContext {
        status_code: None,
        message: Some(format!(
            "The build was not completed within the build timeout of {minutes} minutes, so \
             AWS CodeBuild ended it"
        )),
    }
}

/// The buildspec's `env/variables` key: "variables … you want to make available".
const ENV_KEY: &str = "env";
/// See [`ENV_KEY`].
const VARIABLES_KEY: &str = "variables";
/// See [`ENV_KEY`]: "the variables whose values you want to export".
const EXPORTED_VARIABLES_KEY: &str = "exported-variables";

/// The buildspec's `phases` key, and the members of a phase this reading uses.
const PHASES_KEY: &str = "phases";
/// See [`PHASES_KEY`].
const COMMANDS_KEY: &str = "commands";
/// See [`PHASES_KEY`]: the `install` phase's "runtime-versions … specify the runtime and
/// version".
const RUNTIME_VERSIONS_KEY: &str = "runtime-versions";
/// The buildspec key of the phase `runtime-versions` is declared in.
const INSTALL_PHASE_KEY: &str = "install";
/// See [`PHASES_KEY`]: "on-failure … ABORT … CONTINUE".
const ON_FAILURE_KEY: &str = "on-failure";
/// See [`PHASES_KEY`]: a phase's `finally` block, whose "commands … are run after commands
/// in the commands block" and "are run even if a command in the commands block fails".
const FINALLY_KEY: &str = "finally";
/// The buildspec's `artifacts` key, whose `files` are "the locations … that contain the
/// build output artifacts".
const ARTIFACTS_KEY: &str = "artifacts";
/// See [`ARTIFACTS_KEY`]: "the locations of the build output artifacts in the build
/// environment".
const FILES_KEY: &str = "files";
/// See [`ARTIFACTS_KEY`]: the "directory, relative to the original build location, that
/// CodeBuild uses to determine which files … to include in the build output artifact".
const BASE_DIRECTORY_KEY: &str = "base-directory";
/// See [`ARTIFACTS_KEY`]: "if set to yes … the build artifact's paths are discarded and all
/// files are placed in the root".
const DISCARD_PATHS_KEY: &str = "discard-paths";
/// See [`ARTIFACTS_KEY`]: the artifacts "identified by an artifact identifier" each of the
/// build's secondary destinations takes its files from.
const SECONDARY_ARTIFACTS_KEY: &str = "secondary-artifacts";
/// The buildspec's `reports` key, whose sections each declare "the report group name or
/// ARN" one of the build's reports belongs to.
const REPORTS_KEY: &str = "reports";
/// See [`REPORTS_KEY`]: "the format of the report file" a report's declared files are in.
const FILE_FORMAT_KEY: &str = "file-format";
/// The buildspec's `cache` key, whose `paths` are "the locations … that CodeBuild uses to
/// upload and download the cache".
const CACHE_KEY: &str = "cache";
/// See [`CACHE_KEY`].
const PATHS_KEY: &str = "paths";
/// The `on-failure` value that lets the build go on to the next phase; the other,
/// `ABORT`, is the default and ends the build.
const ON_FAILURE_CONTINUE: &str = "CONTINUE";
/// The value a buildspec's boolean members are declared ON with, beside YAML's own `true`
/// — `discard-paths: yes`, as the buildspec reference writes it.
const YES: &str = "yes";

/// The `EnvironmentVariable$type` whose value IS the value the build sees. The other
/// two name a parameter or a secret the build would resolve through another service,
/// which this reading does not do, so a variable of those types sets nothing here.
///
/// Read by the SANDBOX's session too ([`super::sandbox_shell`]), which gives its commands
/// the same environment variables under the same rule.
pub(super) const PLAINTEXT_VARIABLE_TYPE: &str = "PLAINTEXT";

/// A buildspec as the parts of it a build runs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Buildspec {
    /// The `env/variables` the buildspec declares, in declaration order.
    pub variables: Vec<(String, String)>,
    /// The names in `env/exported-variables`, whose values the build reports once it
    /// has run.
    pub exported_variables: Vec<String>,
    /// The runtimes the install phase's `runtime-versions` selects, as the runtime and the
    /// version each entry names, in declaration order.
    pub runtime_versions: Vec<(String, String)>,
    /// The four phases a build runs the commands of, in the order it runs them — present
    /// for each whether or not the document declares it ([`COMMAND_PHASES`]).
    pub phases: Vec<DeclaredPhase>,
    /// The `artifacts` section, which is what says WHICH files of the build's working tree
    /// the declared artifact is made of — `None` when the buildspec declares none at all.
    pub artifacts: Option<ArtifactDeclaration>,
    /// The `artifacts/secondary-artifacts` sections, by the artifact identifier each is
    /// declared under, which is what addresses one of the build's secondary destinations.
    pub secondary_artifacts: BTreeMap<String, ArtifactDeclaration>,
    /// The `cache/paths` the buildspec declares, which is what a build carries over into
    /// the next build of the same project.
    pub cache_paths: Vec<String>,
    /// The `reports` sections the buildspec declares, by the report group each one names —
    /// which is what makes a build mint a report run per group ([`super::build_reports`]).
    pub reports: BTreeMap<String, ReportDeclaration>,
}

/// One phase of a buildspec: the `BuildPhaseType` it is reported as, the command lines
/// it declares, and whether a failure of one of them ends the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPhase {
    /// The modelled `BuildPhaseType` this phase is reported as.
    pub phase_type: &'static str,
    /// The command lines this phase declares, in declaration order.
    pub commands: Vec<String>,
    /// The command lines the phase's `finally` block declares, in declaration order:
    /// "the commands in a finally block are run even if a command in the commands block
    /// fails".
    pub finally: Vec<String>,
    /// Whether a failing command ends the build — `on-failure: ABORT`, the default,
    /// against `CONTINUE`, which goes on to the next phase.
    pub abort_on_failure: bool,
}

/// What one of a build's artifact destinations is made of, as the buildspec declares it:
/// which files of the working tree, and under which path each is stored.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ArtifactDeclaration {
    /// The `files` patterns, in declaration order.
    pub files: Vec<String>,
    /// The `base-directory` the patterns are matched under, and which is itself not part
    /// of the artifact — `None` when the patterns are matched from the build's own root.
    pub base_directory: Option<String>,
    /// Whether `discard-paths` asks for every matched file to be stored at the root of the
    /// artifact rather than under the path it was produced at.
    pub discard_paths: bool,
}

/// Read `buildspec` as the phases a build runs, or WHY it could not be read: what the
/// YAML parser answered about a text that is not YAML, and [`UnreadableBuildspec::NotRead`]
/// for one this crate does not read inside the bounds it parses a buildspec in.
///
/// A readable buildspec that declares no `phases`, or a phase that declares no
/// `commands`, declares no command: the build then has nothing to run in that phase,
/// which is a phase that runs to success rather than one that failed.
pub fn read_buildspec(buildspec: &str) -> Result<Buildspec, UnreadableBuildspec> {
    let document = buildspec_document(buildspec)?;
    let root = document.as_hash().ok_or(UnreadableBuildspec::NotRead)?;
    let mut read = Buildspec {
        artifacts: mapping_get(root, ARTIFACTS_KEY)
            .and_then(::yaml_rust2::Yaml::as_hash)
            .map(read_artifacts),
        ..Buildspec::default()
    };
    // A buildspec that declares an `artifacts` section this reading finds no mapping in
    // still declares one: what it names is read as nothing rather than as absent.
    if read.artifacts.is_none() && mapping_get(root, ARTIFACTS_KEY).is_some() {
        read.artifacts = Some(ArtifactDeclaration::default());
    }
    if let Some(artifacts) = mapping_get(root, ARTIFACTS_KEY).and_then(::yaml_rust2::Yaml::as_hash)
    {
        if let Some(secondary) =
            mapping_get(artifacts, SECONDARY_ARTIFACTS_KEY).and_then(::yaml_rust2::Yaml::as_hash)
        {
            for (identifier, declared) in secondary {
                let Some(identifier) = identifier.as_str() else {
                    continue;
                };
                let Some(declared) = declared.as_hash() else {
                    continue;
                };
                read.secondary_artifacts
                    .insert(identifier.to_string(), read_artifacts(declared));
            }
        }
    }
    // Each `reports` section declares WHICH files of the working tree hold that group's
    // results and the format they are in; which files those are is the same selection an
    // `artifacts` section declares, so it is read the same way.
    if let Some(reports) = mapping_get(root, REPORTS_KEY).and_then(::yaml_rust2::Yaml::as_hash) {
        for (group, declared) in reports {
            let Some(group) = scalar_string(group) else {
                continue;
            };
            let Some(declared) = declared.as_hash() else {
                continue;
            };
            read.reports.insert(
                group,
                ReportDeclaration {
                    files: read_artifacts(declared),
                    file_format: mapping_get(declared, FILE_FORMAT_KEY).and_then(scalar_string),
                },
            );
        }
    }
    if let Some(cache) = mapping_get(root, CACHE_KEY).and_then(::yaml_rust2::Yaml::as_hash) {
        if let Some(paths) = mapping_get(cache, PATHS_KEY).and_then(::yaml_rust2::Yaml::as_vec) {
            read.cache_paths = paths.iter().filter_map(scalar_string).collect();
        }
    }
    if let Some(env) = mapping_get(root, ENV_KEY).and_then(::yaml_rust2::Yaml::as_hash) {
        if let Some(variables) =
            mapping_get(env, VARIABLES_KEY).and_then(::yaml_rust2::Yaml::as_hash)
        {
            read.variables = variables
                .iter()
                .filter_map(|(name, value)| {
                    Some((name.as_str()?.to_string(), scalar_string(value)?))
                })
                .collect();
        }
        if let Some(exported) =
            mapping_get(env, EXPORTED_VARIABLES_KEY).and_then(::yaml_rust2::Yaml::as_vec)
        {
            read.exported_variables = exported.iter().filter_map(scalar_string).collect();
        }
    }
    // Every one of the four phases is read, whether or not the document declares it: the
    // build RUNS all four ([`COMMAND_PHASES`]), and a phase the buildspec says nothing
    // about is one that runs no command.
    let declared = mapping_get(root, PHASES_KEY).and_then(::yaml_rust2::Yaml::as_hash);
    for (key, phase_type) in COMMAND_PHASES {
        let phase = declared
            .and_then(|phases| mapping_get(phases, key))
            .and_then(::yaml_rust2::Yaml::as_hash);
        let commands = phase
            .and_then(|phase| mapping_get(phase, COMMANDS_KEY))
            .and_then(::yaml_rust2::Yaml::as_vec)
            .map(|listed| listed.iter().filter_map(scalar_string).collect())
            .unwrap_or_default();
        let abort_on_failure = phase
            .and_then(|phase| mapping_get(phase, ON_FAILURE_KEY))
            .and_then(scalar_string)
            .is_none_or(|on_failure| !on_failure.eq_ignore_ascii_case(ON_FAILURE_CONTINUE));
        if key == INSTALL_PHASE_KEY {
            if let Some(runtimes) = phase
                .and_then(|phase| mapping_get(phase, RUNTIME_VERSIONS_KEY))
                .and_then(::yaml_rust2::Yaml::as_hash)
            {
                read.runtime_versions = runtimes
                    .iter()
                    .filter_map(|(runtime, version)| {
                        Some((runtime.as_str()?.to_string(), scalar_string(version)?))
                    })
                    .collect();
            }
        }
        read.phases.push(DeclaredPhase {
            phase_type,
            commands,
            finally: phase
                .and_then(|phase| mapping_get(phase, FINALLY_KEY))
                .and_then(::yaml_rust2::Yaml::as_vec)
                .map(|listed| listed.iter().filter_map(scalar_string).collect())
                .unwrap_or_default(),
            abort_on_failure,
        });
    }
    Ok(read)
}

/// One `artifacts` (or `secondary-artifacts/<identifier>`) section, as the files it names
/// and where they are stored.
fn read_artifacts(declared: &::yaml_rust2::yaml::Hash) -> ArtifactDeclaration {
    ArtifactDeclaration {
        files: mapping_get(declared, FILES_KEY)
            .and_then(::yaml_rust2::Yaml::as_vec)
            .map(|listed| listed.iter().filter_map(scalar_string).collect())
            // A single scalar is one pattern: `files: out.txt` names the same one file
            // `files: [out.txt]` does.
            .or_else(|| {
                mapping_get(declared, FILES_KEY)
                    .and_then(scalar_string)
                    .map(|only| vec![only])
            })
            .unwrap_or_default(),
        base_directory: mapping_get(declared, BASE_DIRECTORY_KEY)
            .and_then(scalar_string)
            .map(|directory| directory.trim().trim_matches('/').to_string())
            .filter(|directory| !directory.is_empty()),
        discard_paths: declared_on(mapping_get(declared, DISCARD_PATHS_KEY)),
    }
}

/// Whether a buildspec member that takes a yes/no answer is ON: `true` as YAML reads it,
/// or the `yes` the buildspec reference writes those members with.
fn declared_on(declared: Option<&::yaml_rust2::Yaml>) -> bool {
    match declared {
        Some(::yaml_rust2::Yaml::Boolean(answer)) => *answer,
        Some(other) => scalar_string(other).is_some_and(|written| {
            written.eq_ignore_ascii_case(YES) || written.eq_ignore_ascii_case("true")
        }),
        None => false,
    }
}

/// What the source-download phase reports about a buildspec it could not read.
///
/// A document the parser REFUSED is reported as the parser answered it — its own
/// diagnostic, unclassified, which is what names the construct and the line a caller has
/// to fix. CodeBuild's own `YAML_FILE_ERROR` classification is what it states about a
/// buildspec it could not obtain or make a specification of at all.
fn unreadable_context(unreadable: &UnreadableBuildspec) -> PhaseContext {
    match unreadable {
        UnreadableBuildspec::Diagnostic(diagnostic) => PhaseContext {
            status_code: None,
            message: Some(diagnostic.clone()),
        },
        UnreadableBuildspec::NotRead => yaml_file_error_context(UNREADABLE_YAML_MESSAGE),
    }
}

/// The context CodeBuild's `YAML_FILE_ERROR` classification is reported with.
fn yaml_file_error_context(message: &str) -> PhaseContext {
    PhaseContext {
        status_code: Some(YAML_FILE_ERROR.to_string()),
        message: Some(message.to_string()),
    }
}

/// What the source-download phase reports about a runtime selection the build environment
/// could not satisfy.
///
/// A runtime NO build image has is the buildspec naming something that is not a runtime, so
/// it carries the same classification as a buildspec CodeBuild could not make a
/// specification of; a version of a known runtime the environment does not have is an
/// installation that could not be performed, which is reported as the command failure it is.
fn runtime_install_context(failure: &RuntimeInstallFailure) -> PhaseContext {
    match failure {
        RuntimeInstallFailure::UnknownRuntime(message) => yaml_file_error_context(message),
        RuntimeInstallFailure::UnknownVersion(message) => PhaseContext {
            status_code: Some(COMMAND_EXECUTION_ERROR.to_string()),
            message: Some(message.clone()),
        },
    }
}

/// What running a build answered: its terminal status, the phase it ends in, the phases
/// it reports and the variables its buildspec exported.
#[derive(Debug, Clone)]
pub struct BuildRun {
    /// The build's terminal `buildStatus`.
    pub status: String,
    /// The phases the build reports, in the order it reached them.
    pub phases: Vec<BuildPhase>,
    /// The variables the buildspec's `exported-variables` named and the build's session
    /// held a value for — `None` when it exported none.
    pub exported_variables: Option<Vec<ExportedEnvironmentVariable>>,
    /// Whether the build ENVIRONMENT was provisioned at all, which is what decides whether
    /// the build reports the environment it RAN IN
    /// ([`super::StoredBuild::record_run`]) — `false` for a build whose provisioning phase
    /// failed, since there was no environment for it to run in.
    pub environment_provisioned: bool,
    /// Epoch millis the build's LAST phase ended at, on the build's own clock
    /// ([`BuildClock`]): the instant the build itself ended, which is what its `endTime`
    /// reports and what every phase window it reports lies inside.
    pub ended_millis: i64,
}

/// What the build environment already holds when the build's phases run, and why the
/// source it would have run on could not be obtained.
///
/// A build's INPUT is the environment's to put there before the first command runs: the
/// source archive the build downloaded and the paths it restored from its cache are files
/// the commands then read. A source the environment could not obtain leaves the build
/// nothing to run at all, and what the phase that fetches it reports is why
/// ([`BuildSeed::source_failure`]).
#[derive(Debug, Default, Clone)]
pub struct BuildSeed {
    /// The files the environment holds, as `(path, contents)`.
    pub files: Vec<(String, String)>,
    /// What the source-download phase reports instead of running the build — `None` when
    /// there was a source to obtain, or none to obtain at all.
    pub source_failure: Option<PhaseContext>,
    /// The value the environment resolved for each variable that names a value another
    /// service holds ([`super::build_secrets`]), by the variable's own name: substituted for
    /// the reference when the session is given its variables.
    pub resolved_variables: BTreeMap<String, String>,
}

/// What running one build's COMMANDS answered, before what they produced is put where the
/// build's artifacts say.
///
/// The run and the transfer are two steps because they answer to different things: what
/// the commands did is the build environment's answer, and where their output went is the
/// object store's ([`finish_build`] joins them into the phases the build reports).
#[derive(Debug, Clone)]
pub struct CommandRun {
    /// Whether any phase that ran failed.
    pub failed: bool,
    /// The phases the build reports so far, in the order it reached them.
    pub phases: Vec<BuildPhase>,
    /// Whether the build got far enough to have output to put anywhere. A build whose
    /// specification could not be read, or whose runtimes could not be installed, never
    /// reached its own finishing work, so it reports no upload phase at all — and neither
    /// does one that was ENDED before it ran its build phase ([`Self::aborted`]), since the
    /// work after that point was never attempted.
    pub reached_output: bool,
    /// Whether a failure in a phase that PREPARES the build ([`PREPARING_PHASES`]), or the
    /// build's execution timeout, ENDED the build: `on-failure: ABORT` (the default) stops
    /// the build at such a phase, and a build whose execution timeout fell due is ended at
    /// the phase that was running then, so every phase after it — the later buildspec phases
    /// and the upload of the build's artifacts — is one the build never attempted, and the
    /// build reports none of them. The build's own finishing work still runs, so
    /// [`finish_build`] still reports that.
    pub aborted: bool,
    /// The buildspec it ran, which is what declares WHICH files its artifacts and its
    /// cache are made of — `None` for a build that never got one.
    pub spec: Option<Buildspec>,
    /// The files the build's working tree holds now, as `(path, contents)` in path order.
    pub produced: Vec<(String, String)>,
    /// What the build's commands WROTE, as the lines its log carries
    /// ([`super::build_shell::BuildShell::written`]) — empty for a build whose commands never
    /// ran.
    pub written: Vec<String>,
    /// See [`BuildRun::exported_variables`].
    pub exported_variables: Option<Vec<ExportedEnvironmentVariable>>,
    /// Epoch millis the last phase that RAN ended at, on the build's own clock
    /// ([`BuildClock`]) — where the build's own finishing work then begins.
    pub ended_millis: i64,
}

/// The phases every build reports before its environment is provisioned: it was submitted,
/// and it waited in the queue until the instant the clock owed its run at.
fn leading_phases(start_millis: i64, due_millis: i64) -> Vec<BuildPhase> {
    vec![
        finished_phase(
            PHASE_SUBMITTED,
            BUILD_STATUS_SUCCEEDED,
            start_millis,
            start_millis,
            None,
        ),
        finished_phase(
            PHASE_QUEUED,
            BUILD_STATUS_SUCCEEDED,
            start_millis,
            due_millis,
            None,
        ),
    ]
}

/// What a build whose ENVIRONMENT could not be provisioned reports: the phase that provisions
/// it FAILED, carrying `context`, and no phase after it ran.
///
/// A build environment is provisioned before the build's source is fetched and before any
/// command of any phase runs, so a build that never got one has no `DOWNLOAD_SOURCE` phase, no
/// command phase and no finishing work: it reports the phases it reached and the terminal
/// marker, exports nothing, and puts nothing anywhere. What the variables its environment
/// could not be given are is [`super::build_secrets`]'s to answer.
pub fn environment_failure(start_millis: i64, due_millis: i64, context: PhaseContext) -> BuildRun {
    let mut phases = leading_phases(start_millis, due_millis);
    phases.push(finished_phase(
        PHASE_PROVISIONING,
        BUILD_STATUS_FAILED,
        due_millis,
        due_millis,
        Some(vec![context]),
    ));
    phases.push(open_phase(PHASE_COMPLETED, due_millis));
    BuildRun {
        status: BUILD_STATUS_FAILED.to_string(),
        phases,
        exported_variables: None,
        environment_provisioned: false,
        // Nothing of this build ran, so its last phase ended where the environment it was
        // to be given failed to appear.
        ended_millis: due_millis,
    }
}

/// One build's own clock while its phases run: where the build stands, and when its
/// execution timeout falls due.
///
/// # The rule it implements
///
/// A phase's `durationInSeconds` is "how long, in seconds, between the starting and ending
/// times of the build's phase", so the window a phase reports has to be the window that
/// phase OCCUPIED — and what a phase occupies is the time its own commands took. This
/// session performs those commands in process and spends none of the host's clock on them,
/// so the time they took is the time they STATE they took
/// ([`super::build_shell::BuildShell::slept_millis`]): the clock advances by that stated
/// time, and the phase windows the build reports partition it in order, each one beginning
/// where the one before it ended.
///
/// The same clock decides the build's execution timeout: `Build$timeoutInMinutes` is "how
/// long, in minutes, from 5 to 2160 (36 hours), for CodeBuild to wait before timing out this
/// build if it does not get marked as completed", counted from the instant the build began
/// executing — the queue has its own wait, `queuedTimeoutInMinutes`. A build still running
/// at the deadline is ended there, so the clock never reports an instant past it.
#[derive(Debug)]
struct BuildClock {
    /// Epoch millis the build began EXECUTING at, which is the instant its environment was
    /// provisioned at.
    started_millis: i64,
    /// Epoch millis the build stands at now, where the next phase begins.
    at_millis: i64,
    /// Epoch millis the build's execution timeout falls due at.
    deadline_millis: i64,
    /// The wait that deadline came from, which is what the phase it ends reports.
    timeout_minutes: i32,
}

impl BuildClock {
    /// The clock of a build that began executing at `started_millis`, with the wait
    /// `build`'s own record states.
    ///
    /// `Build$timeoutInMinutes` is the project's wait with `StartBuild`'s
    /// `timeoutInMinutesOverride` already applied, so the record carries the EFFECTIVE one;
    /// a record that states none is given the default AWS documents for a project
    /// ([`super::DEFAULT_BUILD_TIMEOUT_MINUTES`]), which is the value `CreateProject`
    /// stores when the caller omits one.
    fn new(build: &Build, started_millis: i64) -> Self {
        let timeout_minutes = build
            .timeout_in_minutes
            .unwrap_or(super::DEFAULT_BUILD_TIMEOUT_MINUTES);
        let wait = i64::from(timeout_minutes.max(0)).saturating_mul(super::MILLIS_PER_MINUTE);
        Self {
            started_millis,
            at_millis: started_millis,
            deadline_millis: started_millis.saturating_add(wait),
            timeout_minutes,
        }
    }

    /// Advance to the instant `shell`'s commands have now stated the build stands at, and
    /// answer whether the build is still inside its execution timeout.
    ///
    /// The instant is clamped at the deadline: a build the timeout ENDED stands where it was
    /// ended, because no phase of it occupied the time after that.
    fn advance(&mut self, shell: &BuildShell) -> bool {
        let stated = self.started_millis.saturating_add(shell.slept_millis());
        self.at_millis = stated.min(self.deadline_millis);
        stated <= self.deadline_millis
    }
}

/// Run `build`'s buildspec over the environment `seed` describes and answer what its
/// commands did.
///
/// `start_millis` is the instant the build was submitted and `due_millis` the instant the
/// clock owed its transition out of the queue at, so the queue closes at `due_millis` and
/// the build begins EXECUTING there: every phase that then ran occupies the time its own
/// commands took, on the build's own clock ([`BuildClock`]).
///
/// `tools` are the tools the build environment carries ([`super::build_tools`]), whose calls
/// this build's commands really make; an environment given none has no program for them, so
/// a command naming one is one the session could not run.
pub fn run_commands(
    build: &Build,
    start_millis: i64,
    due_millis: i64,
    seed: BuildSeed,
    tools: Option<super::BuildTools>,
) -> CommandRun {
    let mut phases = leading_phases(start_millis, due_millis);
    // The build environment this emulator provides is created in process, which is what the
    // provisioning phase is (see `super::build_shell`), and it was created: a build whose
    // environment could not be is not run at all ([`environment_failure`]).
    phases.push(finished_phase(
        PHASE_PROVISIONING,
        BUILD_STATUS_SUCCEEDED,
        due_millis,
        due_millis,
        empty_contexts(),
    ));

    let buildspec = build
        .source
        .as_ref()
        .and_then(|source| source.buildspec.as_deref())
        .map(str::trim)
        .filter(|buildspec| !buildspec.is_empty());
    let read = match buildspec {
        None => Err(yaml_file_error_context(NO_YAML_FILE_MESSAGE)),
        Some(buildspec) => {
            read_buildspec(buildspec).map_err(|unreadable| unreadable_context(&unreadable))
        }
    };
    // Whatever the specification says, a build whose own SOURCE could not be obtained has
    // nothing to run: that is the work of the same phase, and it is reported first.
    let read = match seed.source_failure {
        Some(failure) => Err(failure),
        None => read,
    };
    let read = match read {
        Ok(read) => read,
        Err(context) => {
            // The build could not be prepared from its source and its specification, so
            // no phase of it ran: it fails where CodeBuild reports that.
            phases.push(finished_phase(
                PHASE_DOWNLOAD_SOURCE,
                BUILD_STATUS_FAILED,
                due_millis,
                due_millis,
                Some(vec![context]),
            ));
            return CommandRun {
                failed: true,
                phases,
                reached_output: false,
                aborted: false,
                spec: None,
                produced: Vec::new(),
                written: Vec::new(),
                exported_variables: None,
                ended_millis: due_millis,
            };
        }
    };
    // The runtimes the buildspec selected are installed from what the build environment
    // has, in the phase that prepares the build from its specification: a selection it
    // cannot satisfy leaves the build nothing to run, reported as
    // [`runtime_install_context`] states.
    let installed = match install_runtimes(&read.runtime_versions) {
        Ok(installed) => installed,
        Err(failure) => {
            phases.push(finished_phase(
                PHASE_DOWNLOAD_SOURCE,
                BUILD_STATUS_FAILED,
                due_millis,
                due_millis,
                Some(vec![runtime_install_context(&failure)]),
            ));
            return CommandRun {
                failed: true,
                phases,
                reached_output: false,
                aborted: false,
                spec: None,
                produced: Vec::new(),
                written: Vec::new(),
                exported_variables: None,
                ended_millis: due_millis,
            };
        }
    };
    // The buildspec was obtained and read, and the source it runs on is in the working
    // tree, which is the work of this phase.
    phases.push(finished_phase(
        PHASE_DOWNLOAD_SOURCE,
        BUILD_STATUS_SUCCEEDED,
        due_millis,
        due_millis,
        empty_contexts(),
    ));

    let mut shell = BuildShell::new(
        session_variables(build, &read, start_millis, &seed.resolved_variables),
        super::BuildPrograms::new(installed, tools),
        // The build runs INSIDE its effective environment's image, so what that image
        // presents is in the environment's filesystem before the first command runs.
        &image_files(effective_image(build)),
    );
    for (path, contents) in &seed.files {
        // A file the environment could not hold is one the commands will not find: the
        // tree's own bounds are what refuse it, and nothing claims it is there.
        if !shell.put_file(path, contents) {
            ::tracing::debug!(
                %path,
                "a build's working tree could not hold a file the environment seeded"
            );
        }
    }
    let mut failed = false;
    let mut aborted = false;
    // The build began executing when it left the queue, and every phase below occupies the
    // time its own commands took, measured on this clock ([`BuildClock`]).
    let mut clock = BuildClock::new(build, due_millis);
    for declared in &read.phases {
        let phase_start_millis = clock.at_millis;
        let reported = match run_phase(&mut shell, declared, &mut clock) {
            PhaseOutcome::Ran(reported) => reported,
            PhaseOutcome::TimedOut => {
                // The build's execution timeout fell due while this phase was running, so
                // CodeBuild ended the build HERE: this phase is the one that timed out, it
                // ends at the deadline, and the phases after it were never attempted.
                failed = true;
                aborted = true;
                phases.push(finished_phase(
                    declared.phase_type,
                    PHASE_STATUS_TIMED_OUT,
                    phase_start_millis,
                    clock.at_millis,
                    Some(vec![timed_out_context(clock.timeout_minutes)]),
                ));
                break;
            }
        };
        let phase_failed = reported.is_some();
        failed |= phase_failed;
        phases.push(finished_phase(
            declared.phase_type,
            if phase_failed {
                BUILD_STATUS_FAILED
            } else {
                BUILD_STATUS_SUCCEEDED
            },
            phase_start_millis,
            clock.at_millis,
            phase_contexts(reported),
        ));
        // A phase whose failure ENDS the build is the last phase that ran, so the build
        // reports none of the ones after it; `on-failure: CONTINUE` goes on to the next
        // phase, however many phases have already failed, and a failure from the build
        // phase on stops neither the phases after it nor the build's own upload
        // ([`PREPARING_PHASES`]).
        if phase_failed && declared.abort_on_failure && failure_ends_the_build(declared.phase_type)
        {
            aborted = true;
            break;
        }
        // `exit` ended the command block of the phase that just ran, not the build: the
        // next phase's commands are their own program, so the session they run in has not
        // exited, and whether the build reaches them is the question the `on-failure` above
        // already answered. Forgotten here exactly as it is before a `finally` block
        // ([`run_phase`]).
        shell.clear_exit();
    }

    CommandRun {
        failed,
        phases,
        // A build that ran its commands has output to put where its artifacts say, whether
        // those commands succeeded or not: CodeBuild uploads what a build produced for a
        // FAILED build exactly as for one that succeeded.
        //
        // A build that was ENDED before its build phase ran is the other case: it produced
        // nothing, so putting output anywhere is work it never attempted, and it reports no
        // upload phase at all ([`CommandRun::aborted`]).
        reached_output: !aborted,
        aborted,
        produced: shell
            .files()
            .into_iter()
            .map(|(path, contents)| (path.to_string(), contents.to_string()))
            .collect(),
        written: shell.written(),
        exported_variables: exported_variables(&shell, &read),
        spec: Some(read),
        // Where the last phase that ran left the build's clock, which is where its own
        // finishing work begins.
        ended_millis: clock.at_millis,
    }
}

/// The phases a build ENDS with, once what it produced has been put where its artifacts
/// say: the build's own finishing work, and the terminal marker phase.
///
/// `upload` is what the transfer answered — `None` for one that put the build's output
/// where it belongs (which includes a build with nothing to put anywhere), and the context
/// CodeBuild reports for one that could not.
///
/// A build that a failure in one of the phases that PREPARE it ENDED, or that its execution
/// timeout ended, attempted no upload at all, so it reports no `UPLOAD_ARTIFACTS` phase —
/// its own finishing work still ran, which is the phase it does report before the terminal
/// marker.
///
/// The finishing phases are stamped where the build's clock stands
/// ([`CommandRun::ended_millis`]): they come after every phase that ran, so the windows the
/// build reports stay in order and none of them overlaps.
pub fn finish_build(run: CommandRun, upload: Option<PhaseContext>) -> BuildRun {
    let CommandRun {
        mut failed,
        mut phases,
        reached_output,
        aborted,
        exported_variables,
        ended_millis,
        ..
    } = run;
    if reached_output {
        let upload_failed = upload.is_some();
        failed |= upload_failed;
        phases.push(finished_phase(
            PHASE_UPLOAD_ARTIFACTS,
            if upload_failed {
                BUILD_STATUS_FAILED
            } else {
                BUILD_STATUS_SUCCEEDED
            },
            ended_millis,
            ended_millis,
            phase_contexts(upload),
        ));
    }
    // The build's own finishing work runs for every build that got as far as running its
    // commands — the one it aborted at among them.
    if reached_output || aborted {
        phases.push(finished_phase(
            PHASE_FINALIZING,
            BUILD_STATUS_SUCCEEDED,
            ended_millis,
            ended_millis,
            empty_contexts(),
        ));
    }
    phases.push(open_phase(PHASE_COMPLETED, ended_millis));
    BuildRun {
        status: if failed {
            BUILD_STATUS_FAILED.to_string()
        } else {
            BUILD_STATUS_SUCCEEDED.to_string()
        },
        phases,
        exported_variables,
        // This build's commands RAN, so the environment they ran in was provisioned
        // ([`run_commands`] is the only caller, and a build whose environment was not
        // provisioned answers through [`environment_failure`] instead).
        environment_provisioned: true,
        ended_millis,
    }
}

/// What running one phase of a buildspec came to.
#[derive(Debug, Clone)]
enum PhaseOutcome {
    /// Every command the phase declares ran: `Some` is the context of the first that
    /// failed, and `None` is the phase having succeeded.
    Ran(Option<PhaseContext>),
    /// The build's execution timeout fell due while this phase was running, so the build
    /// was ended here and the commands after that instant never ran.
    TimedOut,
}

/// Run one phase's command lines in `shell`, and answer what the phase came to.
///
/// A phase stops at its first failing command ("CodeBuild runs each command, one at a
/// time, in the order listed"), and at an `exit` that ended the session. Its `finally`
/// block then runs whatever the commands block answered — "the commands in a finally block
/// are run even if a command in the commands block fails" — as the program it is, so an
/// `exit` that ended the commands block does not end it too.
///
/// A phase the build's execution timeout fell due in runs nothing further, `finally` block
/// included: the build was ended at that instant, so no command of it ran afterwards.
fn run_phase(
    shell: &mut BuildShell,
    declared: &DeclaredPhase,
    clock: &mut BuildClock,
) -> PhaseOutcome {
    let failure = match run_command_block(shell, &declared.commands, clock) {
        Ok(failure) => failure,
        Err(TimedOut) => return PhaseOutcome::TimedOut,
    };
    if declared.finally.is_empty() {
        return PhaseOutcome::Ran(failure);
    }
    shell.clear_exit();
    match run_command_block(shell, &declared.finally, clock) {
        // The phase failed if either block failed, and what it reports is the FIRST failure:
        // the command a client has to fix is the one that failed first.
        Ok(finally_failure) => PhaseOutcome::Ran(failure.or(finally_failure)),
        Err(TimedOut) => PhaseOutcome::TimedOut,
    }
}

/// That the build's execution timeout fell due while a block of commands was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimedOut;

/// Run one block of command lines in `shell` — a phase's `commands` or its `finally` — and
/// answer the context of the first command that failed, or that the build's execution
/// timeout ended the block.
///
/// The build's clock is advanced after each command by the time that command took, and the
/// timeout is decided on it BEFORE the command's status is read: a command still running at
/// the deadline is one the build was ended in the middle of, so it answered nothing and the
/// commands after it never started.
fn run_command_block(
    shell: &mut BuildShell,
    commands: &[String],
    clock: &mut BuildClock,
) -> Result<Option<PhaseContext>, TimedOut> {
    for command in commands {
        let status = shell.run(command);
        if !clock.advance(shell) {
            return Err(TimedOut);
        }
        if status != 0 {
            return Ok(Some(PhaseContext {
                status_code: Some(COMMAND_EXECUTION_ERROR.to_string()),
                message: Some(command_failure_message(command, status)),
            }));
        }
        if shell.exit_status().is_some() {
            return Ok(None);
        }
    }
    Ok(None)
}

/// The image this build's commands run in: its effective environment's `image`, which is
/// the project's with `StartBuild`'s `imageOverride` already applied — empty for a build
/// whose record carries no environment at all.
fn effective_image(build: &Build) -> &str {
    build
        .environment
        .as_ref()
        .map(|environment| environment.image.trim())
        .unwrap_or_default()
}

/// The Region the build `build` RUNS IN, read off its own ARN
/// (`arn:<partition>:codebuild:<region>:<account>:build/<id>`) — empty for a record that
/// carries no ARN to read one from.
///
/// It is what the build's own commands are told the Region is: the AWS CLI a command invokes
/// takes the Region out of the environment when the command names none.
fn effective_region(build: &Build) -> String {
    build
        .arn
        .as_deref()
        .and_then(|arn| ::service_common::arn::parse(arn).map(|parsed| parsed.region.to_string()))
        .unwrap_or_default()
}

/// The variables one build's session starts with.
///
/// In the order AWS resolves them, so a later one replaces an earlier one of the same
/// name: the variables CodeBuild itself sets for the build, which are read off the
/// build's own record; then the buildspec's `env/variables`; then the build's
/// `environment.environmentVariables`, which `StartBuild`'s overrides are already
/// applied to.
///
/// A variable whose type is not [`PLAINTEXT_VARIABLE_TYPE`] names a parameter or a
/// secret held by another service, so what it is worth is the value the ENVIRONMENT
/// resolved for it before the build ran (`resolved`, [`super::build_secrets`]): a type
/// nothing was resolved for sets nothing rather than setting the reference as if it were
/// the value.
fn session_variables(
    build: &Build,
    read: &Buildspec,
    start_millis: i64,
    resolved: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut variables = BTreeMap::new();
    let image = effective_image(build);
    let region = effective_region(build);
    let in_a_region = (!region.is_empty()).then(|| region.clone());
    let provided: [(&str, Option<String>); 9] = [
        // "AWS_DEFAULT_REGION — The Amazon Web Services Region where the build is running"
        // and its `AWS_REGION` spelling, which CodeBuild sets in the build environment: the
        // CLI a command invokes reads the Region it acts in from them.
        ("AWS_DEFAULT_REGION", in_a_region.clone()),
        ("AWS_REGION", in_a_region),
        ("CODEBUILD_BUILD_ID", build.id.clone()),
        ("CODEBUILD_BUILD_ARN", build.arn.clone()),
        (
            "CODEBUILD_BUILD_NUMBER",
            build.build_number.map(|number| number.to_string()),
        ),
        // The image the build is RUNNING in, which is the one its own commands execute
        // inside: see [`super::build_image`].
        (
            BUILD_IMAGE_VARIABLE,
            (!image.is_empty()).then(|| image.to_string()),
        ),
        ("CODEBUILD_INITIATOR", build.initiator.clone()),
        ("CODEBUILD_START_TIME", Some(start_millis.to_string())),
        ("CODEBUILD_SOURCE_VERSION", build.source_version.clone()),
    ];
    for (name, value) in provided {
        if let Some(value) = value {
            variables.insert(name.to_string(), value);
        }
    }
    for (name, value) in &read.variables {
        variables.insert(name.clone(), value.clone());
    }
    for variable in build
        .environment
        .as_ref()
        .and_then(|environment| environment.environment_variables.as_deref())
        .unwrap_or_default()
    {
        let plaintext = variable
            .r#type
            .as_deref()
            .is_none_or(|kind| kind == PLAINTEXT_VARIABLE_TYPE);
        let value = if plaintext {
            Some(variable.value.clone())
        } else {
            resolved.get(&variable.name).cloned()
        };
        if let Some(value) = value {
            variables.insert(variable.name.clone(), value);
        }
    }
    variables
}

/// The variables the buildspec's `exported-variables` named, with the values the build's
/// session ended holding — `None` when the buildspec named none or the session held a
/// value for none of them.
fn exported_variables(
    shell: &BuildShell,
    read: &Buildspec,
) -> Option<Vec<ExportedEnvironmentVariable>> {
    let exported: Vec<ExportedEnvironmentVariable> = read
        .exported_variables
        .iter()
        .filter_map(|name| {
            Some(ExportedEnvironmentVariable {
                name: Some(name.clone()),
                value: Some(shell.variable(name)?.to_string()),
            })
        })
        .collect();
    (!exported.is_empty()).then_some(exported)
}

#[cfg(test)]
mod tests {
    use super::{
        finish_build, run_commands, BuildSeed, BUILD_STATUS_FAILED, BUILD_STATUS_SUCCEEDED,
        PHASE_STATUS_TIMED_OUT,
    };
    use crate::ops::types::{Build, BuildPhase, ProjectSource};

    /// The instants the store would hand a run: the build was submitted here, and it left the
    /// queue (and so began executing) a settle span later.
    const STARTED_MILLIS: i64 = 1_700_000_000_000;
    /// See [`STARTED_MILLIS`].
    const DUE_MILLIS: i64 = STARTED_MILLIS + super::super::BUILD_SETTLE_MILLIS;

    /// A build whose inline buildspec is `spec` and whose execution timeout is `minutes` —
    /// `None` leaving the record without one, which is the documented default.
    fn build_with(spec: &str, minutes: Option<i32>) -> Build {
        Build {
            source: Some(ProjectSource {
                r#type: "NO_SOURCE".to_string(),
                buildspec: Some(spec.to_string()),
                ..ProjectSource::default()
            }),
            timeout_in_minutes: minutes,
            ..Build::default()
        }
    }

    /// A buildspec declaring `commands` for each phase named, in buildspec order.
    fn buildspec(phases: &[(&str, &str)]) -> String {
        let mut spec = String::from("version: 0.2\nphases:\n");
        for (phase, command) in phases {
            spec.push_str(&format!("  {phase}:\n    commands:\n      - {command}\n"));
        }
        spec
    }

    /// Run `spec` as a whole build — the commands and then the build's own finishing work,
    /// with nothing to put anywhere — and answer the phases it reports and its status.
    fn ran(spec: &str, minutes: Option<i32>) -> (String, Vec<BuildPhase>) {
        let build = build_with(spec, minutes);
        let run = run_commands(&build, STARTED_MILLIS, DUE_MILLIS, BuildSeed::default(), None);
        let finished = finish_build(run, None);
        (finished.status, finished.phases)
    }

    /// The `(phaseType, phaseStatus, durationInSeconds)` of each phase, in the order the
    /// build reached them.
    fn reported(phases: &[BuildPhase]) -> Vec<(String, Option<String>, Option<i64>)> {
        phases
            .iter()
            .map(|phase| {
                (
                    phase.phase_type.clone().unwrap_or_default(),
                    phase.phase_status.clone(),
                    phase.duration_in_seconds,
                )
            })
            .collect()
    }

    /// One row of the duration table: the case, the buildspec the build runs, and the
    /// `durationInSeconds` each named phase must report.
    type DurationRow = (&'static str, String, Vec<(&'static str, i64)>);

    /// RULE: each phase's window is the time ITS OWN commands took, so `durationInSeconds` is
    /// the span between that phase's `startTime` and `endTime` and the phase whose commands
    /// took the longest is the one that reports the longest.
    ///
    /// The rows differ along what the rule generalises over: which phase takes the time, how
    /// much of it, the form the command states it in, how many phases take time at once, and
    /// a build no command of which takes any.
    #[test]
    fn a_phases_duration_is_the_span_of_its_own_window() {
        // (case, the buildspec, the seconds each named phase must report)
        let rows: Vec<DurationRow> = vec![
            (
                "the build phase takes twenty-four seconds",
                buildspec(&[
                    ("install", "echo installing"),
                    ("pre_build", "echo before"),
                    ("build", "sleep 24"),
                    ("post_build", "echo after"),
                ]),
                vec![
                    ("INSTALL", 0),
                    ("PRE_BUILD", 0),
                    ("BUILD", 24),
                    ("POST_BUILD", 0),
                ],
            ),
            (
                "the install phase is the slow one",
                buildspec(&[("install", "sleep 7"), ("build", "echo building")]),
                vec![("INSTALL", 7), ("BUILD", 0)],
            ),
            (
                "a wait stated in minutes",
                buildspec(&[("build", "sleep 3m")]),
                vec![("BUILD", 180)],
            ),
            (
                "a fractional wait, which is a whole second of duration",
                buildspec(&[("build", "sleep 2.25")]),
                vec![("BUILD", 2)],
            ),
            (
                "two phases that each take their own time",
                buildspec(&[("pre_build", "sleep 4"), ("build", "sleep 9")]),
                vec![("PRE_BUILD", 4), ("BUILD", 9)],
            ),
            (
                "no command of the build takes any time",
                buildspec(&[("build", "echo building")]),
                vec![("INSTALL", 0), ("BUILD", 0), ("POST_BUILD", 0)],
            ),
        ];

        for (case, spec, expected) in rows {
            let (status, phases) = ran(&spec, None);
            assert_eq!(
                status, BUILD_STATUS_SUCCEEDED,
                "case {case:?}: a build whose commands all succeeded is {BUILD_STATUS_SUCCEEDED}, \
                 got {status} with {:?}",
                reported(&phases)
            );
            // The documented definition, read for every phase that ended: the duration IS the
            // span between the two instants it reports.
            for phase in &phases {
                let name = phase.phase_type.clone().unwrap_or_default();
                let (Some(start), Some(end)) = (phase.start_time, phase.end_time) else {
                    continue;
                };
                let span = (end.to_millis().unwrap_or_default()
                    - start.to_millis().unwrap_or_default())
                    / 1_000;
                assert_eq!(
                    phase.duration_in_seconds,
                    Some(span),
                    "case {case:?}: phase {name} must report the span of its own window \
                     ({span} s), got {phase:?}"
                );
            }
            for (phase_type, seconds) in expected {
                let found = phases
                    .iter()
                    .find(|phase| phase.phase_type.as_deref() == Some(phase_type))
                    .and_then(|phase| phase.duration_in_seconds);
                assert_eq!(
                    found,
                    Some(seconds),
                    "case {case:?}: expected {phase_type} to have taken {seconds} s, got \
                     {:?}",
                    reported(&phases)
                );
            }
        }
    }

    /// RULE: a build still running when its execution timeout falls due is ended there — the
    /// phase that was running carries `TIMED_OUT` with the explanation, the phases after it
    /// are not reported, and the build is `FAILED`. A build inside its wait is untouched.
    ///
    /// The rows differ along what the rule generalises over: which phase is running when the
    /// wait runs out, the length of the wait (the record's own, and the documented default a
    /// record that states none is given), and how far past or short of it the commands run.
    #[test]
    fn the_phase_running_when_the_wait_runs_out_is_the_one_that_timed_out() {
        // (case, the buildspec, the record's timeoutInMinutes, the phase that timed out)
        let rows: Vec<(&str, String, Option<i32>, Option<&str>)> = vec![
            (
                "the build phase runs an hour under a five-minute wait",
                buildspec(&[("build", "sleep 3600"), ("post_build", "echo never")]),
                Some(5),
                Some("BUILD"),
            ),
            (
                "an earlier phase is the one running when the wait runs out",
                buildspec(&[("install", "sleep 1200"), ("build", "echo never")]),
                Some(5),
                Some("INSTALL"),
            ),
            (
                "the documented default wait a record that states none is given",
                buildspec(&[("build", "sleep 4000")]),
                None,
                Some("BUILD"),
            ),
            (
                "a build whose commands finish inside its wait",
                buildspec(&[("build", "sleep 30"), ("post_build", "echo after")]),
                Some(5),
                None,
            ),
            (
                "a build that takes no time at all",
                buildspec(&[("build", "echo building")]),
                Some(5),
                None,
            ),
        ];

        for (case, spec, minutes, timed_out) in rows {
            let (status, phases) = ran(&spec, minutes);
            let table = reported(&phases);
            let Some(timed_out) = timed_out else {
                assert_eq!(
                    status, BUILD_STATUS_SUCCEEDED,
                    "case {case:?}: a build inside its wait runs to success, got {table:?}"
                );
                assert!(
                    !table
                        .iter()
                        .any(|(_, state, _)| state.as_deref() == Some(PHASE_STATUS_TIMED_OUT)),
                    "case {case:?}: no phase of a build inside its wait timed out, got {table:?}"
                );
                continue;
            };
            assert_eq!(
                status, BUILD_STATUS_FAILED,
                "case {case:?}: a build its execution timeout ended is {BUILD_STATUS_FAILED}, got \
                 {table:?}"
            );
            let phase = phases
                .iter()
                .find(|phase| phase.phase_type.as_deref() == Some(timed_out))
                .unwrap_or_else(|| panic!("case {case:?}: expected a {timed_out} phase, got {table:?}"));
            assert_eq!(
                phase.phase_status.as_deref(),
                Some(PHASE_STATUS_TIMED_OUT),
                "case {case:?}: {timed_out} was running when the wait ran out, got {table:?}"
            );
            let explained = phase.contexts.iter().flatten().any(|context| {
                context
                    .message
                    .as_deref()
                    .is_some_and(|message| !message.trim().is_empty())
            });
            assert!(
                explained,
                "case {case:?}: the phase that timed out carries the explanation, got {phase:?}"
            );
            // It ended AT the deadline, so it reports no more time than the wait allowed.
            let allowed = i64::from(minutes.unwrap_or(super::super::DEFAULT_BUILD_TIMEOUT_MINUTES))
                * 60;
            assert_eq!(
                phase.duration_in_seconds,
                Some(allowed),
                "case {case:?}: the phase the wait ended ran from the start of the build's \
                 execution to its deadline ({allowed} s), got {table:?}"
            );
            // And nothing after it was attempted, while the build's own finishing work was.
            for absent in ["POST_BUILD", "UPLOAD_ARTIFACTS"] {
                assert!(
                    !table.iter().any(|(phase, _, _)| phase == absent),
                    "case {case:?}: {absent} was never attempted, got {table:?}"
                );
            }
            for present in ["FINALIZING", "COMPLETED"] {
                assert!(
                    table.iter().any(|(phase, _, _)| phase == present),
                    "case {case:?}: expected {present} to be reported, got {table:?}"
                );
            }
        }
    }
}
