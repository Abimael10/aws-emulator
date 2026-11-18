// SPDX-License-Identifier: CC0-1.0

//! Running the builds the clock owes a run, where the account's object store can be
//! reached.
//!
//! # The rule this module implements
//!
//! A build's run is not only its commands: its INPUT comes out of the object store (the
//! source archive it was pointed at, the cache its project carries) and its OUTPUT goes
//! back into it (the artifact objects the buildspec declared, the cache it leaves for the
//! next build). Those are calls to another service, so a build is run HERE — from an
//! operation, over the emulator's in-process transport — rather than inside the store
//! lock, which can reach nothing.
//!
//! What this module adds is only WHERE the steps happen; the order is the build's own:
//!
//! 0. the stream the build's output is delivered to is provisioned, which is a decision about
//!    the role the build runs as ([`withheld_log_delivery`]) — a build whose role does not let
//!    CodeBuild deliver there reports nothing and runs nothing, because every step after this
//!    one reports what it did into that stream;
//! 1. the variables the build environment is given are resolved, which for a variable naming
//!    a value another service holds is a call to that service ([`super::build_secrets`]) — a
//!    build whose environment cannot be given them runs nothing at all;
//! 2. the source archive is downloaded and unpacked into the working tree, and the cached
//!    paths are restored into it ([`super::build_source`], [`super::build_cache`]);
//! 3. the buildspec's phases run over that tree ([`super::run_commands`]), with the TOOLS the
//!    build environment carries ([`super::build_tools`]), whose calls those commands really
//!    make;
//! 4. what the commands produced is put where the build's artifacts say
//!    ([`super::artifact_upload`]), the report runs its `reports` sections declared are read
//!    from the result files those commands wrote ([`super::build_reports`]), and the declared
//!    cache paths are saved;
//! 5. the build records what every step answered ([`super::finish_build`]).
//!
//! # Why a reporting operation drives it
//!
//! The transition out of `IN_PROGRESS` is owed at the instant the clock made it due
//! whether or not anybody called, so it is materialised by whoever reads the build next —
//! the same rule every other settle in this crate follows. What is different here is only
//! that the run happens OUTSIDE the store's lock: it reads a caller-supplied buildspec and
//! it calls another service, and neither belongs in a critical section every other request
//! of this service shares. A run is CLAIMED out of the store before it starts
//! ([`ComAmazonawsCodebuildCodebuild20161006State::claim_due_build_runs`]) and recorded
//! into it after, so two calls that arrive together run one build once; a build whose run
//! is still in flight is still `IN_PROGRESS`, which is what it is.
//!
//! What one run costs is bounded before it starts: the bytes a source or cache object
//! contributes ([`super::build_source`], [`super::build_cache`]), the entries and the
//! expansion of the archive they arrive in ([`super::source_archive`]), the working tree
//! the commands write, and the command budget every command of the buildspec spends
//! (`::service_common::posix_shell_session`), whatever form that command takes.

use ::smithy_server_runtime::EmbeddedEmulator;

use super::{
    artifact_upload, build_cache, build_secrets, build_source, now_millis, BuildRun, BuildSeed,
    ComAmazonawsCodebuildCodebuild20161006State,
};
use crate::ops::types::{Build, BuildArtifacts, BuildPhase, PhaseContext, ProjectArtifacts};

/// One build whose run the clock owes, claimed out of the store
/// ([`ComAmazonawsCodebuildCodebuild20161006State::claim_due_build_runs`]) by the
/// operation that is performing it.
#[derive(Debug, Clone)]
pub struct ClaimedRun {
    /// The id the store keys this build by, which is the id the build reports.
    pub id: String,
    /// The build's record as it stands, which is what the run is driven from.
    pub build: Build,
    /// The project this build belongs to, read off the record or off the id it is keyed
    /// by, so the cache it carries is scoped to a project rather than to nothing.
    pub project_name: String,
    /// The destination of the build's primary output artifacts.
    pub artifacts: Option<ProjectArtifacts>,
    /// The destinations of its secondary output artifacts.
    pub secondary_artifacts: Vec<ProjectArtifacts>,
    /// Epoch millis the build was started at.
    pub start_millis: i64,
    /// Epoch millis its terminal transition became due at.
    pub due_millis: i64,
    /// Whether this build may restore its project's cache: a project whose cache was
    /// reset after the last save has nothing a build may restore.
    pub cache_restorable: bool,
}

/// Run every build whose run the clock owes and which needs the object store, and record
/// what each one answered.
///
/// Called from an operation that is about to READ or decide on a build, with no guard of
/// the store held: it takes the lock itself, once to claim the runs and once per run to
/// record it, and the steps between them are calls to another emulated service. `engine` is
/// the shared engine those services' own stores live in, which is what the verdicts a build's
/// environment turns on are asked of ([`super::build_secrets`]).
pub async fn settle_due_builds<Eng: Send + Sync + 'static>(
    callback: &EmbeddedEmulator,
    engine: &Eng,
    state: &::engine::ServiceState<ComAmazonawsCodebuildCodebuild20161006State>,
    region: &str,
) {
    let claimed = state.lock().claim_due_build_runs(now_millis());
    let mut completed = Vec::new();
    for run in claimed {
        let recorded = perform(callback, engine, region, &run).await;
        completed.extend(state.lock().record_build_run(&run, region, recorded));
    }
    // What each build that ENDED here is metered by goes into the CloudWatch store of the
    // SAME engine, with this service's own store no longer held: a client watching build
    // health over time reads that series rather than the build record
    // ([`super::build_metrics`]).
    super::build_metrics::publish_completed_builds(engine, &completed);
}

/// What one build's run came to, as the store records it.
#[derive(Debug, Clone)]
pub struct RecordedRun {
    /// What running the build answered: its status, its phases and its exported
    /// variables.
    pub run: BuildRun,
    /// What the build reports for its primary output artifacts once they were put at
    /// their destination — `None` for a build that put none there.
    pub artifacts: Option<BuildArtifacts>,
    /// The same for each of its secondary output artifacts, by the identifier that
    /// addresses one.
    pub secondary_artifacts: Vec<(String, BuildArtifacts)>,
    /// The instant this build's cache was saved at — `None` when it saved none.
    pub cache_saved_millis: Option<i64>,
    /// The report runs this build produced, one per report group its buildspec declared —
    /// empty for a build that declared none, or one that never ran its commands.
    pub reports: Vec<super::ReportRun>,
}

/// What the provisioning phase reports for a build whose command session could not be
/// started at all: the environment it was to run in was not provisioned, so no command of it
/// ran and nothing is claimed about any.
///
/// AWS publishes no sentence for it — a real build environment is a container CodeBuild
/// starts — so it states the condition in this service's own terms.
const SESSION_NOT_STARTED_MESSAGE: &str =
    "The build environment's command session could not be started, so no command of this \
     build ran";

/// Run `claimed`'s buildspec, on a thread of the runtime's BLOCKING pool.
///
/// The session the commands run in is synchronous and the tools its commands use make calls
/// of their own ([`super::build_tools`]), so the commands are performed off the runtime's
/// workers: a call is then driven by blocking that thread on the runtime handle, which a
/// worker may not do. `None` is a session nothing ran, which is reported as a build
/// environment that was not provisioned rather than as commands that did nothing.
async fn performed_commands(
    callback: &EmbeddedEmulator,
    region: &str,
    claimed: &ClaimedRun,
    seed: BuildSeed,
) -> Option<super::CommandRun> {
    // The tools this build's commands are given, carrying the role the build RUNS AS: every
    // call they make is authorized against that role's own permissions. What they write on
    // standard error is kept where this call can read it back, because the build's log
    // carries it ([`super::build_tools::ToolDiagnostics`]).
    let diagnostics = super::build_tools::ToolDiagnostics::default();
    let tools = super::BuildTools::new(
        callback,
        region,
        claimed.build.service_role.as_deref(),
        &claimed.id,
        &diagnostics,
    );
    let build = claimed.build.clone();
    let start_millis = claimed.start_millis;
    let due_millis = claimed.due_millis;
    let mut run = ::tokio::task::spawn_blocking(move || {
        super::run_commands(&build, start_millis, due_millis, seed, tools)
    })
    .await
    .ok()?;
    run.written.extend(diagnostics.lines());
    Some(run)
}

/// The IAM actions CodeBuild performs a build's log delivery under ("Actions defined by Amazon
/// CloudWatch Logs", IAM Service Authorization Reference): the stream the build's output goes
/// to is created, and the build's records are put into it. Both are taken on the stream's own
/// ARN, which is the one the build reports.
const LOG_DELIVERY_ACTIONS: [&str; 2] = ["logs:CreateLogStream", "logs:PutLogEvents"];

/// CodeBuild's own sentence for a build whose service role does not let it deliver the
/// build's output to CloudWatch Logs: the role it was to deliver as, the build it was
/// delivering for, and the refusal the delivery would have been answered with — which names
/// the action that was withheld and the stream it was withheld on.
fn log_delivery_withheld_message(role_arn: &str, build_arn: &str, refusal: &str) -> String {
    format!(
        "Service role {role_arn} does not allow AWS CodeBuild to create Amazon CloudWatch Logs \
         log streams for build {build_arn}. Error message: {refusal}"
    )
}

/// `Some(context)` when the permissions in force on `build`'s service role do NOT let
/// CodeBuild deliver the build's output to the stream the build reports — the context the
/// phase that provisions the build then carries — and `None` when they do, when the build
/// delivers to no stream at all, or when nothing is known about the role.
///
/// A build's log stream is provisioned as CodeBuild starts the build, BEFORE its source is
/// fetched and before any command of any phase runs: the build's own agent reports everything
/// it does into that stream, so a build whose role withholds a delivery action is one CodeBuild
/// does not run at all. The decision is the one every call made for a build goes
/// through ([`super::build_tools::role_withholds`]), asked of the stream's own ARN, so a role
/// that states no policies withholds nothing.
async fn withheld_log_delivery(
    callback: &EmbeddedEmulator,
    region: &str,
    build: &Build,
    build_id: &str,
) -> Option<PhaseContext> {
    let logs = build.logs.as_ref()?;
    // The stream's own ARN is the resource the delivery actions are granted on, and a build
    // whose CloudWatch delivery is DISABLED reports none: nothing is delivered for it, so no
    // permission on a stream is needed.
    let stream_arn = logs.cloud_watch_logs_arn.as_deref()?;
    let role_arn = build.service_role.as_deref()?;
    // The principal a refusal names is the session CodeBuild assumes the role in for this
    // build, the same one every other call made for it is refused as.
    let principal =
        super::role_session_arn(role_arn, build_id).unwrap_or_else(|| role_arn.to_string());
    for action in LOG_DELIVERY_ACTIONS {
        let Some(withheld) =
            super::build_tools::role_withholds(callback, region, role_arn, action, stream_arn).await
        else {
            continue;
        };
        let refusal = ::service_common::iam_authorization::not_authorized_by_identity_policy(
            &principal,
            action,
            stream_arn,
            &withheld.reason(action),
        );
        let message = log_delivery_withheld_message(
            role_arn,
            &super::build_arn(region, build_id),
            &refusal,
        );
        return Some(super::build_source::client_error(&message));
    }
    None
}

/// Deliver what `build`'s commands wrote into the log group and stream the build REPORTS
/// ([`super::build_logs`]).
///
/// A build's output is delivered to CloudWatch Logs as the build runs — which is what makes
/// `logs.groupName`/`logs.streamName` the documented way to read why a build failed — so the
/// records go into the very group and stream the build's own record names, through the
/// workspace's one vended-logs write (`::service_common::cloudwatch_logs`). A build whose
/// delivery is DISABLED names no group or stream and nothing is delivered.
///
/// Whether the build's service role lets CodeBuild deliver there at all is decided before the
/// build runs ([`withheld_log_delivery`]), so a build that got this far has a stream its role
/// may write.
async fn deliver_log(
    callback: &EmbeddedEmulator,
    region: &str,
    build: &Build,
    records: &[String],
) {
    if records.is_empty() {
        return;
    }
    let Some(logs) = build.logs.as_ref() else {
        return;
    };
    let (Some(group), Some(stream)) = (logs.group_name.as_deref(), logs.stream_name.as_deref())
    else {
        return;
    };
    ::service_common::cloudwatch_logs::delivered_over(callback, region, group, stream, records)
        .await;
}

/// The lines the build's AGENT contributes to its log for `phases`: the state each phase it
/// reached ended in, and the context a phase that did not succeed carries.
///
/// A CodeBuild log is not only what the commands wrote: the agent writes the phase
/// transitions and the context of a phase that failed into the same stream, which is what
/// makes the log readable as the account of why a build ended as it did — including a build
/// whose commands never ran, whose whole account is the phase that stopped it. The two lines
/// are the agent's own (`Phase complete: <phase> State: <state>` and `Phase context status
/// code: <code> Message: <message>`); every value in them is the phase's own. A phase whose
/// context reports NOTHING (the empty context a phase that ended cleanly carries) has
/// nothing for the second line to say, so none is written for it.
fn agent_lines(phases: &[BuildPhase]) -> Vec<String> {
    let mut lines = Vec::new();
    for phase in phases {
        let Some(phase_type) = phase.phase_type.as_deref() else {
            continue;
        };
        if let Some(state) = phase.phase_status.as_deref() {
            lines.push(format!("Phase complete: {phase_type} State: {state}"));
        }
        for context in phase.contexts.iter().flatten() {
            let status_code = context.status_code.clone().unwrap_or_default();
            let message = context.message.clone().unwrap_or_default();
            if status_code.is_empty() && message.is_empty() {
                continue;
            }
            lines.push(format!(
                "Phase context status code: {status_code} Message: {message}"
            ));
        }
    }
    lines
}

/// Perform one claimed build's run: its input, its commands, and its output.
async fn perform<Eng: Send + Sync + 'static>(
    callback: &EmbeddedEmulator,
    engine: &Eng,
    region: &str,
    claimed: &ClaimedRun,
) -> RecordedRun {
    let project_name = claimed.project_name.as_str();
    let mut seed = BuildSeed::default();
    // The stream the build's output is delivered to is provisioned before anything of the
    // build happens, because every later step reports what it did into it: a build whose role
    // does not let CodeBuild deliver there gets no environment at all — nothing runs, nothing
    // is delivered, and the phase that provisions it carries why.
    if let Some(context) =
        withheld_log_delivery(callback, region, &claimed.build, &claimed.id).await
    {
        return RecordedRun {
            run: super::environment_failure(claimed.start_millis, claimed.due_millis, context),
            artifacts: None,
            secondary_artifacts: Vec::new(),
            cache_saved_millis: None,
            // Nothing of this build ran, so it produced no result file to read a report run
            // from.
            reports: Vec::new(),
        };
    }
    // The variables the build environment is given come first: they are in place before the
    // first command of any phase runs, and one that names a value another service holds is
    // substituted with that value. An environment that could not be given them is a build
    // that runs nothing — no source is fetched, no command runs and nothing is put anywhere.
    match build_secrets::resolve_environment(callback, engine, region, &claimed.build).await {
        build_secrets::ResolvedEnvironment::Resolved(resolved) => {
            seed.resolved_variables = resolved;
        }
        build_secrets::ResolvedEnvironment::Refused(context) => {
            return RecordedRun {
                run: super::environment_failure(claimed.start_millis, claimed.due_millis, context),
                artifacts: None,
                secondary_artifacts: Vec::new(),
                cache_saved_millis: None,
                // Nothing of this build ran, so it produced no result file to read a report
                // run from.
                reports: Vec::new(),
            };
        }
    }
    // The build's own source, at the version the build was started with: the pinned
    // object when it names one, whatever the object holds now.
    if let Some(source) = claimed.build.source.as_ref() {
        match build_source::download(
            callback,
            region,
            source,
            claimed.build.source_version.as_deref(),
        )
        .await
        {
            build_source::SourceDownload::NotAnObject => {}
            build_source::SourceDownload::Unpacked(files) => seed.files.extend(files),
            build_source::SourceDownload::Refused(context) => {
                seed.source_failure = Some(context);
            }
        }
    }
    // The cache the project carries, unless it was reset after the cache was written: a
    // build started after `InvalidateProjectCache` restores nothing saved before it.
    let cache = claimed.build.cache.clone().unwrap_or_default();
    if claimed.cache_restorable {
        seed.files
            .extend(build_cache::restore(callback, region, &cache, project_name).await);
    }

    let run = match performed_commands(callback, region, claimed, seed).await {
        Some(run) => run,
        None => {
            // The session the build's commands run in could not be started at all, so no
            // command of this build ran: that is the environment this build was to be given
            // not having been provisioned, which is the phase that reports it.
            return RecordedRun {
                run: super::environment_failure(
                    claimed.start_millis,
                    claimed.due_millis,
                    super::build_source::client_error(SESSION_NOT_STARTED_MESSAGE),
                ),
                artifacts: None,
                secondary_artifacts: Vec::new(),
                cache_saved_millis: None,
                reports: Vec::new(),
            };
        }
    };
    // What the commands wrote, and what the agent says about the phases they ran in, goes
    // where the build says its output goes — before anything else is decided about the
    // build: the log is how a client reads WHY a build ended as it did, including a build
    // that ended at the phase that just failed.
    let mut records = run.written.clone();
    records.extend(agent_lines(&run.phases));
    deliver_log(callback, region, &claimed.build, &records).await;

    // The report runs the buildspec declared are read from the result files the commands
    // wrote, which is part of the build's own finishing work: a build a phase failure ABORTED
    // does that work too, so a test report is produced for the build whose tests failed it
    // exactly as for one that passed. A build that never got a specification declared no
    // report at all.
    let reports = run.spec.as_ref().map_or_else(Vec::new, |spec| {
        super::report_runs(&spec.reports, &run.produced)
    });

    // Nothing is put anywhere for a build that never reached its own finishing work.
    if !run.reached_output {
        return RecordedRun {
            run: super::finish_build(run, None, claimed.due_millis),
            artifacts: None,
            secondary_artifacts: Vec::new(),
            cache_saved_millis: None,
            reports,
        };
    }
    let spec = run.spec.clone();
    let produced = run.produced.clone();
    // The id the store keys this build by, which is the id it reports: the destination's
    // namespace is composed from it, so it is never an empty segment.
    let build_id = claimed.id.as_str();

    // The primary destination, and then each secondary one: the same upload, each taking
    // the files the buildspec declared for it.
    let mut refusal = None;
    let mut uploaded = None;
    if let Some(destination) = claimed
        .artifacts
        .as_ref()
        .and_then(|artifacts| artifact_upload::destination_of(artifacts, build_id))
    {
        let reported = claimed.build.artifacts.clone().unwrap_or_default();
        match artifact_upload::upload(
            callback,
            region,
            &destination,
            &reported,
            spec.as_ref().and_then(|spec| spec.artifacts.as_ref()),
            &produced,
        )
        .await
        {
            artifact_upload::ArtifactUpload::Uploaded(artifacts) => uploaded = Some(artifacts),
            artifact_upload::ArtifactUpload::Refused(context) => refusal = Some(context),
        }
    }
    let mut secondary_artifacts = Vec::new();
    for artifacts in &claimed.secondary_artifacts {
        let Some(destination) = artifact_upload::destination_of(artifacts, build_id) else {
            continue;
        };
        // The identifier is what addresses this destination's own files in the buildspec
        // (`artifacts/secondary-artifacts/<identifier>`), so a destination with none, or
        // one the buildspec declares no section for, has nothing declared to upload —
        // which the upload below reports as the client error it is.
        let identifier = artifacts.artifact_identifier.clone().unwrap_or_default();
        let reported = claimed
            .build
            .secondary_artifacts
            .iter()
            .flatten()
            .find(|reported| reported.artifact_identifier.as_deref() == Some(&identifier))
            .cloned()
            .unwrap_or_default();
        match artifact_upload::upload(
            callback,
            region,
            &destination,
            &reported,
            spec.as_ref()
                .and_then(|spec| spec.secondary_artifacts.get(&identifier)),
            &produced,
        )
        .await
        {
            artifact_upload::ArtifactUpload::Uploaded(artifacts) => {
                secondary_artifacts.push((identifier, artifacts));
            }
            // One destination that took nothing fails the phase, exactly as the primary
            // one does: the build did not produce what it declared.
            artifact_upload::ArtifactUpload::Refused(context) => {
                refusal = refusal.or(Some(context));
            }
        }
    }

    // The cache the next build of this project restores is saved by the build that ran,
    // from the paths its own buildspec declared.
    let cached = match spec.as_ref().map(|spec| spec.cache_paths.clone()) {
        Some(paths) if !paths.is_empty() => {
            build_cache::save(callback, region, &cache, project_name, &paths, &produced).await
        }
        _ => false,
    };

    RecordedRun {
        run: super::finish_build(run, refusal, claimed.due_millis),
        artifacts: uploaded,
        secondary_artifacts,
        cache_saved_millis: cached.then_some(claimed.due_millis),
        reports,
    }
}
