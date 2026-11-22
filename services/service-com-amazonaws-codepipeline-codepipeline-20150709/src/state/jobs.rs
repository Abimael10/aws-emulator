// SPDX-License-Identifier: CC0-1.0

//! What a job worker REPORTS: `AcknowledgeJob`, `PutJobSuccessResult` and
//! `PutJobFailureResult`, and what reporting it does to the action the job belongs to.
//!
//! # The rule this module implements
//!
//! `PollForJobs` hands a worker the job of a `Custom`-owned action and
//! [`super::JobRecord`] is that job; this is the other half of the same lifecycle. The
//! worker CLAIMS the job with the nonce its poll published ("a system-generated random
//! number that CodePipeline uses to ensure that the job is being worked on by only one
//! job worker"), and the outcome it then reports IS the action's outcome: a success
//! finishes the action — unless it carries a `continuationToken`, which
//! `PutJobSuccessResult` documents as identifying "a custom action in progress", so the
//! same job goes back to waiting for the worker's next report — and a failure fails the
//! action, which the engine ([`super::driver`]) then rolls up into its stage and its
//! execution.
//!
//! Nothing here invents an outcome: the status an action reaches is the one its worker
//! reported, and the only call that moves a job is one from the worker holding it. What
//! the worker says ALONGSIDE the outcome is its report about that action execution too —
//! the summary, the external id of the run that did the work and how far along it is, and
//! for a failure the type and message it failed with — so each is recorded where the
//! service reports it from ([`record_reported_details`]) instead of being dropped in
//! favour of what the dispatch recorded.
//!
//! # What a job will not take
//!
//! * a job no worker has claimed, or one already completed, takes no result
//!   (`InvalidJobStateException`): there is no worker whose outcome it is — except a
//!   report that RESTATES the result the job already holds, which asks for nothing the
//!   job has not recorded and is accepted as the no-op it is, because what the service
//!   refuses is a job "updated with the same status, but different result or output
//!   variables";
//! * a nonce no dispatch of the job published does not claim it
//!   (`InvalidNonceException`), and neither does another dispatch's nonce once a worker
//!   has claimed the job, which is what makes the claim exclusive — while the job is
//!   still waiting, EVERY nonce a poll published for it claims it, because a redelivery
//!   does not take the job away from the worker that was handed it first, and the nonce
//!   that DID claim it keeps reaching it for as long as the claim lasts, so the worker
//!   may repeat the acknowledgement it is unsure arrived;
//! * a job whose work has ended takes no acknowledgement either, but the condition is
//!   that its receipt was already recorded rather than a nonce that does not claim it,
//!   so it is the validation failure the operation models for exactly that
//!   (`ValidationException`);
//! * a job whose ACTION is no longer running — abandoned by a stop, or finished without
//!   the job reporting it — is not work any more, so it is swept away ([`super::ComAmazonawsCodepipelineCodepipeline20150709State::retire_finished_jobs`])
//!   and a call naming it is answered `JobNotFoundException`, exactly as `PollForJobs`
//!   and `GetJobDetails` answer it. A job the worker itself carried to a terminal
//!   status is KEPT, so the late result above is the state error and
//!   `JobNotFoundException` stays what an id that was never issued is answered with.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{CurrentRevision, ExecutionDetails, FailureDetails};

use super::{
    ComAmazonawsCodepipelineCodepipeline20150709State as State, JOB_STATUS_FAILED,
    JOB_STATUS_IN_PROGRESS, JOB_STATUS_QUEUED, JOB_STATUS_SUCCEEDED, TERMINAL_JOB_STATUSES,
};

/// The temporary credentials a job's worker reads and writes the pipeline's artifacts
/// with, minted in the emulator's own credential store so the emulator accepts the calls
/// a worker signs with them.
///
/// `JobData$artifactCredentials` / `ThirdPartyJobData$artifactCredentials` are "temporary
/// credentials that are issued by Amazon Web Services Secure Token Service" that "can be
/// used to access input and output artifacts in the S3 bucket used to store artifact for
/// the pipeline", and `GetJobDetails` documents that CodePipeline "returns temporary
/// credentials for the S3 bucket used to store artifacts for the pipeline, if the action
/// requires access to that S3 bucket for input or output artifacts" — so a worker that is
/// handed a job carrying artifacts is handed credentials for them, and one whose action
/// touches no artifact is not.
///
/// Neither the id nor the secrets are chosen here: the credential store mints them, which
/// is what makes a request signed with them one this emulator serves. `None` when the
/// store minted a credential without the secrets a caller would have to sign with, in
/// which case the credential is released again and the job is handed out without them
/// rather than with material nothing can use.
///
/// Two limits of what this emulator can mint, stated rather than hidden: the credential
/// store holds no policies, so the session carries no scope of its own — it is not
/// confined to the artifact bucket the way the STS session AWS issues is — and nothing
/// revokes it at the instant a job finishes. Its life is bounded instead:
/// [`super::ARTIFACT_CREDENTIAL_LIFETIME`] after the dispatch the store releases it
/// ([`super::ComAmazonawsCodepipelineCodepipeline20150709State::due_artifact_credentials`]),
/// and a job's credentials are minted once, so a worker polling in a loop leaves nothing
/// behind.
pub fn mint_artifact_credentials(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Option<crate::ops::types::AWSSessionCredentials> {
    use ::emulator_server::aws_credentials::{AWS_SESSION, SECRET_ACCESS_KEY, SESSION_TOKEN};

    let minted = ctx.credential_store().create(&AWS_SESSION);
    let (Some(secret_access_key), Some(session_token)) = (
        minted.secret(SECRET_ACCESS_KEY).map(str::to_string),
        minted.secret(SESSION_TOKEN).map(str::to_string),
    ) else {
        let _ = ctx.credential_store().delete(&minted.id);
        return None;
    };
    Some(crate::ops::types::AWSSessionCredentials {
        access_key_id: minted.id,
        secret_access_key,
        session_token,
    })
}

/// The `ActionExecutionStatus` an action reaches when its worker reports success, from
/// the model's enum (`smithy_query query="shape" shape="ActionExecutionStatus"` ->
/// `InProgress`, `Abandoned`, `Succeeded`, `Failed`).
const ACTION_SUCCEEDED: &str = "Succeeded";

/// The `ActionExecutionStatus` an action reaches when its worker reports failure.
const ACTION_FAILED: &str = "Failed";

/// The most bytes of `outputVariables` one job result may report.
///
/// A bound is required: the variables are kept on the execution and read by its later
/// actions, so without one a worker could grow the store per request. The refusal past it
/// is the error `PutJobSuccessResult` models for exactly this condition —
/// `OutputVariablesSizeExceededException`, "Exceeded the total size limit for all
/// variables in the pipeline". AWS publishes its own variable-size quota per account and
/// adjusts it, so the FIGURE here is this emulator's bound; what is not chosen is that a
/// result past it is refused with that error rather than accepted.
pub const MAX_OUTPUT_VARIABLES_BYTES: usize = 120 * 1024;

/// The most bytes ONE output variable's value may carry.
///
/// A per-variable bound is required as well as the per-result one: an output variable
/// becomes the replacement text of every `#{namespace.key}` placeholder a later action's
/// configuration carries, so without it one 120 KB variable would multiply by the
/// placeholder count of every configuration value the engine and the worker resolve.
/// `ActionConfigurationValue` is modelled `@length(min: 1, max: 1000)`, so a replacement
/// longer than that could not fit a configuration value in the first place: this is that
/// figure, and a result reporting a longer value is refused with the same modelled error.
pub const MAX_OUTPUT_VARIABLE_VALUE_BYTES: usize = 1_000;

/// A job-worker call this service refuses, and the modelled error that states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobProblem {
    /// Refused as `JobNotFoundException`: no job of that id is queued for any action
    /// still waiting for a worker.
    NotFound(String),
    /// Refused as `InvalidNonceException`: the job exists, but the nonce presented is not
    /// the one its newest poll published.
    InvalidNonce(String),
    /// Refused as `InvalidJobStateException`: the job is not one this worker may report a
    /// result for — it has not been claimed, or it has already been completed.
    InvalidState(String),
    /// Refused as `OutputVariablesSizeExceededException`.
    OutputVariablesTooLarge(String),
}

/// Claim the job `job_id` for the worker presenting `nonce`, and answer the status it now
/// carries.
///
/// `AcknowledgeJob` "confirms a job worker has received the specified job", which is what
/// makes that worker the one whose result the job accepts: the job moves to `InProgress`
/// and its claim is no longer open to another worker. The nonce a poll published is the
/// worker's handle on the job for the whole CLAIM WINDOW — from the dispatch that
/// published it until the job's work ends — so three conditions follow:
///
/// * while the job is still waiting, the nonce has to be one a poll published for it —
///   any of them: a job that resurfaced with a fresh nonce is still the job the earlier
///   dispatch handed out, so the earlier nonce claims it just as well, and claiming
///   through one of them is what closes the others;
/// * while the job is being worked on, the nonce that claimed it still reaches it, and
///   the acknowledgement it repeats is the no-op it asks for: the receipt is already
///   recorded, so the job stays exactly where it is and its status is reported again.
///   Another dispatch's nonce reaches it no more — that is what "worked on by only one
///   job worker" means — and is refused `InvalidNonceException`, the answer that tells
///   that worker the job is not its to claim;
/// * once the job's work has ENDED there is no claim window left to confirm a receipt
///   in, and the acknowledgement is refused with the validation failure the operation
///   models for a receipt that was already recorded
///   ([`super::already_acknowledged_message`]) rather than with a nonce refusal: the
///   value the worker presented is the one the service published for it.
pub fn acknowledge(
    state: &mut State,
    job_id: &str,
    nonce: &str,
    now: SystemTime,
) -> Result<String, JobProblem> {
    // A job whose action is no longer waiting for a worker is not a job any more, which
    // is the sweep every job-worker operation performs first.
    state.retire_finished_jobs();
    let job = job_mut(state, job_id, super::job_not_found_message)?;
    if job.has_terminated() {
        return Err(JobProblem::InvalidState(
            super::already_acknowledged_message(job_id),
        ));
    }
    // The worker holding the claim, re-confirming a receipt the job already carries: the
    // status it asks for is the one recorded, so nothing moves.
    if job.is_claimed_with(nonce) {
        return Ok(job.status.clone());
    }
    if job.status != JOB_STATUS_QUEUED || !job.admits_nonce(nonce) {
        return Err(JobProblem::InvalidNonce(super::invalid_nonce_message(
            job_id,
        )));
    }
    job.claim(nonce, now);
    Ok(job.status.clone())
}

/// Record a worker's successful result for `job_id`.
///
/// A result carrying a `continuationToken` identifies an action that is still running —
/// "future jobs use this token to identify the running instance of the action", and "when
/// the action is complete, no continuation token should be supplied" — so the JOB goes
/// back to waiting (carrying that token, for the worker's next report) and the action
/// stays `InProgress`. Without one the action itself succeeds, with the output variables
/// the worker reported. Either way the `executionDetails` the result states are recorded
/// on the action execution ([`record_reported_details`]) and the `currentRevision` it
/// states is filed as the action's: both are what the worker says about the work it has
/// done, so they are reported for a run still in progress as well as for one the result
/// completed. The output variables are the exception — they are what the action
/// PUBLISHED, so a run that has not finished publishing them records none.
pub fn record_success(
    state: &mut State,
    job_id: &str,
    continuation_token: Option<String>,
    current_revision: Option<CurrentRevision>,
    execution_details: Option<ExecutionDetails>,
    output_variables: Option<HashMap<String, String>>,
    now: SystemTime,
) -> Result<(), JobProblem> {
    if let Some(variables) = &output_variables {
        let size: usize = variables
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum();
        if size > MAX_OUTPUT_VARIABLES_BYTES {
            return Err(JobProblem::OutputVariablesTooLarge(
                output_variables_too_large_message(variables),
            ));
        }
        // And one variable on its own: it becomes the replacement text of every
        // placeholder a later action's configuration carries, so its length is bounded at
        // the figure a configuration value can hold at all.
        if let Some((name, value)) = variables
            .iter()
            .find(|(_, value)| value.len() > MAX_OUTPUT_VARIABLE_VALUE_BYTES)
        {
            return Err(JobProblem::OutputVariablesTooLarge(format!(
                "Exceeded the size limit for the variable {name}: {} bytes reported, at most \
                 {MAX_OUTPUT_VARIABLE_VALUE_BYTES} allowed for one variable",
                value.len()
            )));
        }
    }

    state.retire_finished_jobs();
    let reported = super::ReportedJobResult::success(
        continuation_token.as_deref(),
        current_revision.as_ref(),
        execution_details.as_ref(),
        output_variables.as_ref(),
    );
    let mut still_running = false;
    let (execution_id, stage, action, pipeline_name, stage_name, action_name) = {
        let job = job_mut(state, job_id, super::job_result_not_found_message)?;
        if job.status != JOB_STATUS_IN_PROGRESS {
            if restates_result(job, JOB_STATUS_SUCCEEDED, &reported) {
                return Ok(());
            }
            return Err(JobProblem::InvalidState(invalid_state_message(
                job_id,
                &job.status,
                JOB_STATUS_SUCCEEDED,
            )));
        }
        let target = (
            job.pipeline_execution_id.clone(),
            job.stage,
            job.action,
            job.pipeline_name.clone(),
            job.stage_name.clone(),
            job.action_name.clone(),
        );
        if let Some(token) = continuation_token {
            // The action continues on the SAME job: it goes back to waiting, carrying the
            // token, so the worker's next poll is handed the continuing work rather than
            // a second record accumulating for one action.
            job.status = JOB_STATUS_QUEUED.to_string();
            job.release();
            job.continuation_token = Some(token);
            job.last_status_change = now;
            still_running = true;
        } else {
            job.status = JOB_STATUS_SUCCEEDED.to_string();
            job.reported_result = Some(reported);
            job.last_status_change = now;
        }
        target
    };

    // What the worker reported as the revision it worked on, filed where
    // `GetPipelineState` reports an action's current revision from. A result that only
    // states progress reports it too: `currentRevision` is "the ID of the current revision
    // of the artifact successfully worked on by the job", which is a fact about the work
    // done so far rather than about the job ending.
    if let Some(revision) = current_revision {
        state.action_revisions.insert(
            (pipeline_name, stage_name, action_name),
            crate::ops::types::ActionRevision {
                revision_id: revision.revision,
                revision_change_id: revision.change_identifier,
                created: revision
                    .created
                    .unwrap_or_else(|| super::to_date_time(now)),
            },
        );
    }

    // A result that only states PROGRESS settles nothing: the action keeps its
    // `InProgress` status and its outcome is still the worker's to report. What the
    // worker said about the work so far is nonetheless its report about THIS action
    // execution, so it is recorded where the service reports it from — nothing else
    // about the run moves, the status included, so its `lastStatusChange` stands. The
    // output variables are not recorded for it: they are what the action PUBLISHED, and
    // the action has not finished publishing.
    if still_running {
        if let Some(details) = &execution_details {
            if let Some(record) = state.executions.get_mut(&execution_id) {
                if let Some(run) = record
                    .stages
                    .get_mut(stage)
                    .and_then(|stage| stage.actions.get_mut(action))
                {
                    record_reported_details(run, details);
                }
                record.last_update_time = now;
            }
        }
        return Ok(());
    }

    // The namespace the action publishes its output variables under is the DECLARED
    // one, read from the stage of the pinned declaration the execution's plan names.
    let namespace = state.executions.get(&execution_id).and_then(|record| {
        let declaration = state
            .pipelines
            .get(&record.pipeline_name)?
            .declaration_at(Some(record.pipeline_version))?;
        let run = record.stages.get(stage)?;
        super::execution::declared_action(declaration, run, action)?
            .namespace
            .clone()
    });

    let Some(record) = state.executions.get_mut(&execution_id) else {
        return Err(JobProblem::NotFound(super::job_result_not_found_message(
            job_id,
        )));
    };
    let Some(run) = record
        .stages
        .get_mut(stage)
        .and_then(|stage| stage.actions.get_mut(action))
    else {
        return Err(JobProblem::NotFound(super::job_result_not_found_message(
            job_id,
        )));
    };
    run.status = Some(ACTION_SUCCEEDED.to_string());
    if let Some(details) = &execution_details {
        record_reported_details(run, details);
    }
    run.claimed = None;
    run.last_status_change = now;
    record.last_update_time = now;

    // The variables the worker published are this action execution's own output, so they
    // are recorded on the RUN — which is where `ActionExecutionOutput$outputVariables` is
    // reported from, for an action declaring a namespace and for one declaring none
    // alike. The values are kept exactly as the result stated them: they are opaque
    // strings, so nothing here re-encodes, trims or converts one.
    if let Some(variables) = &output_variables {
        run.output_variables = variables
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }

    // An action's output variables are read by the later actions of the same execution
    // through the namespace that action declares, which is the namespace the engine
    // resolves `#{namespace.key}` placeholders against.
    if let (Some(namespace), Some(variables)) = (namespace, output_variables) {
        for (name, value) in variables {
            record
                .variables_map
                .insert(format!("{namespace}.{name}"), value);
        }
    }
    Ok(())
}

/// Record a worker's failure for `job_id`: the job and the ACTION it reports on both
/// fail, carrying the `failureDetails` the worker gave — its `type` and `message` as the
/// action's error details and summary, and the `externalExecutionId` it named as the
/// action execution's external id — and the next advance fails the action's stage and the
/// execution with it.
pub fn record_failure(
    state: &mut State,
    job_id: &str,
    failure: &FailureDetails,
    now: SystemTime,
) -> Result<(), JobProblem> {
    state.retire_finished_jobs();
    let reported = super::ReportedJobResult::failure(failure);
    let (execution_id, stage, action) = {
        let job = job_mut(state, job_id, super::job_result_not_found_message)?;
        if job.status != JOB_STATUS_IN_PROGRESS {
            if restates_result(job, JOB_STATUS_FAILED, &reported) {
                return Ok(());
            }
            return Err(JobProblem::InvalidState(invalid_state_message(
                job_id,
                &job.status,
                JOB_STATUS_FAILED,
            )));
        }
        job.status = JOB_STATUS_FAILED.to_string();
        job.reported_result = Some(reported);
        job.last_status_change = now;
        (job.pipeline_execution_id.clone(), job.stage, job.action)
    };

    let Some(record) = state.executions.get_mut(&execution_id) else {
        return Err(JobProblem::NotFound(super::job_result_not_found_message(
            job_id,
        )));
    };
    let Some(run) = record
        .stages
        .get_mut(stage)
        .and_then(|stage| stage.actions.get_mut(action))
    else {
        return Err(JobProblem::NotFound(super::job_result_not_found_message(
            job_id,
        )));
    };
    run.status = Some(ACTION_FAILED.to_string());
    run.summary = Some(failure.message.clone());
    // `FailureDetails$type` is the `FailureType` the worker chose for this failure, so it
    // is reported as the action's error code rather than replaced by one of ours, and
    // `FailureDetails$message` is the worker's OWN statement about the work it performed:
    // the party that failed published it, so every reader of the action reports it
    // ([`super::execution::ActionFailure::provider_summary`]).
    run.error = Some(super::execution::ActionFailure {
        code: failure.r#type.clone(),
        message: failure.message.clone(),
        provider_summary: false,
    });
    // `FailureDetails$externalExecutionId` is "the external ID of the run of the action
    // that failed", which is exactly what the action execution reports as its provider's
    // external id: a worker that named the run it failed in is reported with THAT id
    // rather than with the one the dispatch recorded.
    if let Some(external_id) = &failure.external_execution_id {
        run.external_execution_id = Some(external_id.clone());
    }
    run.claimed = None;
    run.last_status_change = now;
    record.last_update_time = now;
    Ok(())
}

/// Record on the action execution `run` what a worker's result STATED about the work it
/// performed.
///
/// `ExecutionDetails` is "the details of the actions taken and results produced on an
/// artifact as it passes through stages in the pipeline", and the action execution is
/// where each of its members is reported back: `summary` as the provider's summary for
/// the action execution (`ActionExecutionResult$externalExecutionSummary`, and
/// `ActionExecution$summary` beside it), `externalExecutionId` — "the system-generated
/// unique ID of this action used to identify this job worker in any external systems" —
/// as `ActionExecutionResult$externalExecutionId`, "the action provider's external ID for
/// the action execution", and `percentComplete` as `ActionExecution$percentComplete`.
/// Nothing here is computed: each value is the one the worker gave.
///
/// Only what the report STATES is written. A member the worker left out asks for no
/// change, so what the run already carries about the work — the summary and the external
/// id whatever started that work recorded, and the figure an earlier progress report gave
/// — stands rather than being erased by a result that said nothing about it.
fn record_reported_details(run: &mut super::ActionRun, details: &ExecutionDetails) {
    if let Some(summary) = &details.summary {
        run.summary = Some(summary.clone());
    }
    if let Some(external_id) = &details.external_execution_id {
        run.external_execution_id = Some(external_id.clone());
    }
    if let Some(percent) = details.percent_complete {
        run.percent_complete = Some(percent);
    }
}

/// The job queued under `job_id`, or the refusal a call naming no job answers, worded
/// as `not_found` states it.
///
/// The lookup is one rule for every job-worker operation; which words its refusal
/// carries belongs to the CALLING operation's family
/// ([`super::job_not_found_message`] for the acknowledge/details family,
/// [`super::job_result_not_found_message`] for the put-result family), so the wording
/// travels in rather than being fixed here.
fn job_mut<'a>(
    state: &'a mut State,
    job_id: &str,
    not_found: fn(&str) -> String,
) -> Result<&'a mut super::JobRecord, JobProblem> {
    state
        .jobs
        .values_mut()
        .flatten()
        .find(|job| job.job_id == job_id)
        .ok_or_else(|| JobProblem::NotFound(not_found(job_id)))
}

/// Whether `job` already holds exactly the result being reported: it reached `desired`
/// by a worker reporting THIS statement, so the report restates what the job carries.
///
/// The condition CodePipeline refuses is a job "updated with the same status, but
/// different result or output variables" — so a report that changes nothing about the
/// job is not that condition: the outcome the worker is telling the service about is
/// already the recorded one, which is what makes a worker's retry of a report it is
/// unsure reached the service safe. A report of the same status carrying any other
/// result, and one of a DIFFERENT status, are both still refused.
fn restates_result(
    job: &super::JobRecord,
    desired: &str,
    reported: &super::ReportedJobResult,
) -> bool {
    job.status == desired && job.reported_result.as_ref() == Some(reported)
}

/// The most characters a message of this service carries: `Message` — the member every
/// one of its errors states its message in — is modelled `@length(min: 1, max: 5000)`
/// (`smithy_query query="shape" shape="Message"`).
const MAX_MESSAGE_LENGTH: usize = 5_000;

/// What `OutputVariablesSizeExceededException` states: the limit the result went past,
/// and what each variable of THIS result measured.
///
/// The limit and the fixed text are constants; the breakdown names every variable the
/// request carried with its measured size, so the worker reading the refusal can tell
/// WHICH of them spent the budget. A variable's size is its name plus its value, which
/// is what the total is summed from, and the entries are in name order so one request
/// is stated one way.
///
/// The breakdown is one entry per variable the REQUEST carried, and the map it comes
/// from is modelled with no bound on how many that is, so the statement is held to the
/// length its member is modelled with ([`MAX_MESSAGE_LENGTH`]): the entries that fit are
/// named, and a result carrying thousands of variables cannot grow the refusal past the
/// member that has to carry it.
fn output_variables_too_large_message(variables: &HashMap<String, String>) -> String {
    let mut measured: Vec<(&str, usize)> = variables
        .iter()
        .map(|(name, value)| (name.as_str(), name.len() + value.len()))
        .collect();
    measured.sort_unstable();
    let mut message = format!(
        "The total size of all output variables for this action exceeds the limit of \
         {MAX_OUTPUT_VARIABLES_BYTES} bytes. Output variables (in bytes): ["
    );
    for (position, (name, bytes)) in measured.iter().enumerate() {
        let separator = if position == 0 { "" } else { ", " };
        let entry = format!("{separator}{name}: {bytes}");
        // The closing bracket is part of the statement, so it is counted with the entry
        // that would precede it.
        if message.len() + entry.len() + 1 > MAX_MESSAGE_LENGTH {
            break;
        }
        message.push_str(&entry);
    }
    message.push(']');
    message
}

/// Why a job cannot take a result: it is not one a worker holds and has still to
/// complete. Built from the request (the `jobId`) and the job's own status, against the
/// status the result being reported would move it to (`desired`).
///
/// Three conditions, each with its own statement:
///
/// * a job already carrying the status this result reports cannot be updated with the
///   same status and a different result — the outcome it holds is already that one;
/// * a job in any other terminal status has terminated, so its outcome is settled;
/// * a job that is not terminal and not `InProgress` is one no worker has acknowledged,
///   so there is no worker whose outcome it would be.
///
/// One source for every operation that refuses a RESULT on the job's state, whichever
/// plane the worker reports on: `PutJobSuccessResult`/`PutJobFailureResult` here, and
/// the partner plane's `PutThirdPartyJobSuccessResult`/`PutThirdPartyJobFailureResult`,
/// which model the same `InvalidJobStateException` for the same three conditions, so
/// neither plane words one of them for itself. The statement names the job the REQUEST
/// named and the condition it is in — never the `JobStatus` the store holds, which the
/// service does not disclose here.
pub fn invalid_state_message(job_id: &str, status: &str, desired: &str) -> String {
    if status == desired {
        return format!(
            "The status of job with id {job_id} is already set to the desired value. It cannot \
             be updated with the same status, but different result or output variables."
        );
    }
    if TERMINAL_JOB_STATUSES.contains(&status) {
        return format!("Job with id {job_id} has already terminated");
    }
    format!("Job with id {job_id} has not been acknowledged")
}
