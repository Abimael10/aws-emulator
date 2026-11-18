// SPDX-License-Identifier: CC0-1.0

//! How a stored IoT job is PROJECTED onto the `Job` shape this model declares: what its
//! targets have made of it, and which members a reader of one always reports.
//!
//! The job itself lives in the shared store ([`super::job_store`]); this module is the
//! only place that turns one into a `Job`, so every reader of a job here (`DescribeJob`,
//! and whatever else projects the same record) reports one value for one record.

use super::*;

/// A configuration structure as every reader of this service's JOBS and JOB TEMPLATES
/// reports one: the configuration the record holds, or the EMPTY structure for a record
/// that holds none.
///
/// AWS IoT materialises the configuration of every job and job template it keeps, so a
/// reader reports the structure whether or not anything configured it and a structure
/// with no members in it is how "nothing is configured" is reported. This is the one
/// implementation of that rule, so a reader of a job and a reader of a job template
/// cannot differ about it.
///
/// It applies to a structure every one of whose members is optional. One the model gives
/// a `@required` member (`AbortConfig$criteriaList`,
/// `JobExecutionsRetryConfig$criteriaList`, both `length 1..`) has no empty form at all,
/// so it is reported only from a record that holds one and does NOT come through here.
pub fn reported_configuration<Configuration: Default>(
    stored: Option<Configuration>,
) -> Option<Configuration> {
    Some(stored.unwrap_or_default())
}

/// What a job's TARGETS have made of it: the `JobStatus` it reports, when it was
/// completed, and the per-status counts `jobProcessDetails` carries.
///
/// It is COUNTED from the executions in the shared store rather than stored a second
/// time on the job, so what a reader of the job says and what a reader of one device's
/// execution says cannot disagree.
#[derive(Debug, Clone)]
pub struct JobProgress {
    /// The `JobStatus` to report.
    pub status: String,
    /// When the job was completed, in epoch seconds — only for a job that IS
    /// completed, since `Job$completedAt` is "the time … when the job was completed".
    pub completed_at: Option<i64>,
    /// `Job$jobProcessDetails`.
    pub details: JobProcessDetails,
    /// `Job$isConcurrent` — "Indicates whether a job is concurrent. Will be true when a
    /// job is rolling out new job executions or canceling previously created executions,
    /// otherwise false": the `CONTINUOUS` job is the one with executions still to roll
    /// out.
    pub is_concurrent: bool,
}

/// `jobProcessDetails` before a single execution has been counted into it: every
/// per-status counter at ZERO.
///
/// Each member of `JobProcessDetails` other than `processingTargets` is "the number of
/// things" in one `JobExecutionStatus`, so a job whose executions include none in that
/// status has a count of that many — zero — rather than an unknown. `processingTargets`
/// is the one member documented as absent ("this value will be null after the job
/// execution has finished rolling out to all the target devices"), so it alone starts
/// unset.
fn counted_from_no_executions() -> JobProcessDetails {
    JobProcessDetails {
        processing_targets: None,
        number_of_canceled_things: Some(0),
        number_of_succeeded_things: Some(0),
        number_of_failed_things: Some(0),
        number_of_rejected_things: Some(0),
        number_of_queued_things: Some(0),
        number_of_in_progress_things: Some(0),
        number_of_removed_things: Some(0),
        number_of_timed_out_things: Some(0),
    }
}

/// How far a job's targets have got with it, from the executions the shared store
/// holds.
///
/// The job's own `status` decides while it is TERMINAL — a cancelled job rolls out to
/// nothing further, whatever its executions say. Otherwise a SNAPSHOT job that has
/// executions and no PENDING one is `COMPLETED`: `Job$targetSelection` documents such a
/// job as one that "will be complete after all those things specified as targets have
/// completed the job". A `CONTINUOUS` job "will continue to run", and a job whose
/// creator named no selection documents neither, so both keep the status their creator
/// recorded.
///
/// `processingTargets` is "the target devices to which the job execution is being
/// rolled out. This value will be null after the job execution has finished rolling out
/// to all the target devices", so it lists the things whose execution is still pending
/// and is left out once none is.
pub fn job_progress(job: &RegisteredJob, executions: &JobExecutionStore) -> JobProgress {
    let mut details = counted_from_no_executions();
    let mut processing: Vec<String> = Vec::new();
    let (mut total, mut pending) = (0usize, 0usize);
    let mut finished_at: Option<i64> = None;
    for record in executions.executions_of_job(&job.job_id) {
        total += 1;
        // The modelled `JobExecutionStatus` values
        // (`super::types::JOB_EXECUTION_STATUS_VALUES`), each counted into the member
        // `JobProcessDetails` declares for it.
        let bucket = match record.status.as_str() {
            "QUEUED" => &mut details.number_of_queued_things,
            "IN_PROGRESS" => &mut details.number_of_in_progress_things,
            "SUCCEEDED" => &mut details.number_of_succeeded_things,
            "FAILED" => &mut details.number_of_failed_things,
            "REJECTED" => &mut details.number_of_rejected_things,
            "TIMED_OUT" => &mut details.number_of_timed_out_things,
            "CANCELED" => &mut details.number_of_canceled_things,
            "REMOVED" => &mut details.number_of_removed_things,
            // A status outside the enum counts towards no bucket rather than towards
            // the wrong one.
            _ => continue,
        };
        *bucket = Some(bucket.unwrap_or(0).saturating_add(1));
        if record.is_pending() {
            pending += 1;
            processing.push(record.thing_name.clone());
        } else {
            finished_at = Some(
                finished_at
                    .unwrap_or(record.last_updated_at)
                    .max(record.last_updated_at),
            );
        }
    }
    details.processing_targets = (!processing.is_empty()).then_some(processing);

    let completed = !job.is_terminal() && job.is_snapshot() && total > 0 && pending == 0;
    let status = if completed {
        iot_jobs::JOB_STATUS_COMPLETED.to_string()
    } else {
        job.status.clone()
    };
    // `Job$isConcurrent`: "Indicates whether a job is concurrent. Will be true when a job
    // is rolling out new job executions or canceling previously created executions,
    // otherwise false." A `CONTINUOUS` job is the one that keeps rolling out new
    // executions: it "will continue to run" and "may also be run on a thing when a change
    // is detected in a target" (`Job$targetSelection`), so new executions are still to
    // come whatever the job's own status says. A `SNAPSHOT` job — and a job whose creator
    // named no selection — rolls out once to the targets it was created with, which here
    // is done by the time the create answers, so it rolls out nothing new.
    let is_concurrent = job.target_selection.as_deref()
        == Some(::service_common::iot_jobs::TARGET_SELECTION_CONTINUOUS);
    JobProgress {
        status,
        // The job was completed when its last execution finished.
        completed_at: completed.then_some(finished_at).flatten(),
        details,
        is_concurrent,
    }
}

/// A stored job as the modeled `Job` this plane reports.
///
/// The shared store holds the job; this is the only place that projects it onto the
/// shapes THIS model declares, so every reader of a job here reports one value for one
/// record — whichever service minted it.
///
/// The members a job CARRIES come from the record, and the members its rollout has
/// REACHED from the executions ([`job_progress`]).
///
/// The CONFIGURATION STRUCTURES a job is deployed under — `presignedUrlConfig`,
/// `jobExecutionsRolloutConfig`, `schedulingConfig` and `timeoutConfig` — are every one
/// of them reported, carrying the values the record holds and NO member where it holds
/// none ([`reported_configuration`]). `abortConfig` and `jobExecutionsRetryConfig` are
/// the two whose own `criteriaList` the model makes `@required`, so there is no empty
/// form of either and they are reported only from a record that holds criteria — which
/// none here does. `jobTemplateArn`, `namespaceId` and `destinationPackageVersions` are
/// single values no creator persists, and `scheduledJobRollouts` is "the next seven
/// maintenance window occurrences" of a schedule no record carries, so those are left
/// out rather than reported empty.
///
/// `forceCanceled` is "true if the job was canceled with the optional force parameter
/// set to true", and `reasonCode` / `comment` are what the call that moved the job's own
/// lifecycle stated about it, so all three come from the stored record and are reported
/// only once something cancelled the job — a job nobody cancelled carries none of them.
pub fn to_job(job: &RegisteredJob, executions: &JobExecutionStore) -> Job {
    let progress = job_progress(job, executions);
    Job {
        job_arn: Some(job.job_arn.clone()),
        job_id: Some(job.job_id.clone()),
        target_selection: job.target_selection.clone(),
        status: Some(progress.status),
        force_canceled: job.force_canceled,
        reason_code: job.reason_code.clone(),
        comment: job.comment.clone(),
        // `JobTargets` is modelled `len 1..`, so a job with no stored target reports
        // no list rather than an empty one.
        targets: (!job.targets.is_empty()).then(|| job.targets.clone()),
        description: job.description.clone(),
        // The configuration structures, reported for every job: nothing persists a job's
        // pre-signed URL, rollout or scheduling configuration here (`CreateJob` documents
        // why), so those three carry no member at all.
        presigned_url_config: reported_configuration(None),
        job_executions_rollout_config: reported_configuration(None),
        abort_config: None,
        created_at: Some(::aws_smithy_types::DateTime::from_secs(job.created_at)),
        last_updated_at: Some(::aws_smithy_types::DateTime::from_secs(job.last_updated_at)),
        completed_at: progress
            .completed_at
            .map(::aws_smithy_types::DateTime::from_secs),
        job_process_details: Some(progress.details),
        timeout_config: reported_configuration(Some(TimeoutConfig {
            in_progress_timeout_in_minutes: job.in_progress_timeout_minutes,
        })),
        namespace_id: None,
        job_template_arn: None,
        job_executions_retry_config: None,
        document_parameters: (!job.document_parameters.is_empty()).then(|| {
            job.document_parameters
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }),
        is_concurrent: Some(progress.is_concurrent),
        scheduling_config: reported_configuration(None),
        scheduled_job_rollouts: None,
        destination_package_versions: None,
    }
}
