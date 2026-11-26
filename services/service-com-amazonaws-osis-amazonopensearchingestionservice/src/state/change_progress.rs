// SPDX-License-Identifier: CC0-1.0

//! How far the change a pipeline is going through has got, as `GetPipelineChangeProgress`
//! reports it.
//!
//! That operation "returns progress information for the current change happening on an
//! OpenSearch Ingestion pipeline. Currently, this operation only returns information when a
//! pipeline is being created" (the operation's own documentation), so the ONE change this
//! service reports on is the create of the pipeline: it started when the record was minted,
//! it is in progress while the pipeline has not come up yet, it has completed once it has,
//! and it failed where the create did.
//!
//! A change is reported after it has FINISHED, not only while it runs: the status enum a
//! reported change carries (smithy_query: shape ChangeProgressStatuses) carries the two
//! terminal values COMPLETED and FAILED, which no reading that stopped reporting a change once
//! it settled could ever produce. That is what settles the create of a pipeline past its
//! create being reported as the completed change rather than as no change at all.
//!
//! Both facts it states are read off the pipeline record — the instant it was created and the
//! status the clock has settled it into — so this report cannot disagree with what
//! `GetPipeline` says about the same pipeline.

use ::std::time::SystemTime;

use crate::ops::types::ChangeProgressStatus;

/// `com.amazonaws.osis#ChangeProgressStatuses` values a reported change carries.
///
/// The enum (smithy_query: shape ChangeProgressStatuses) is PENDING, IN_PROGRESS, COMPLETED,
/// FAILED. A create this service holds a record for has always begun — the record is minted
/// by the call that starts it — so no change is reported as `PENDING`.
pub const CHANGE_IN_PROGRESS: &str = "IN_PROGRESS";
/// The change came up: the pipeline reached a status past its create.
pub const CHANGE_COMPLETED: &str = "COMPLETED";
/// The change did not come up: the create left the pipeline failed.
pub const CHANGE_FAILED: &str = "FAILED";

/// The create of a pipeline in `pipeline_status` since `created_at`, as a change in progress.
///
/// The progress IS the pipeline's own status, which is what the create moves: a pipeline
/// still in `CREATING` is a create in progress, one the create left in `CREATE_FAILED` is a
/// create that failed, and every other `PipelineStatus` is past the create, which therefore
/// completed.
///
/// No per-stage breakdown is reported: neither the model nor its documentation names a stage
/// of a pipeline create, so the change goes through no stage this service can name — the
/// count is the number of stages it reports (none) and the list is empty, rather than stage
/// names and descriptions nothing derived.
pub fn create_change_progress(pipeline_status: &str, created_at: SystemTime) -> ChangeProgressStatus {
    let status = match pipeline_status {
        super::STATUS_CREATING => CHANGE_IN_PROGRESS,
        super::STATUS_CREATE_FAILED => CHANGE_FAILED,
        _ => CHANGE_COMPLETED,
    };
    ChangeProgressStatus {
        start_time: Some(::aws_smithy_types::DateTime::from(created_at)),
        status: Some(status.to_string()),
        total_number_of_stages: 0,
        change_progress_stages: Some(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the reported progress of a pipeline's create is the pipeline's own status — in
    /// progress while it is being created, failed where the create failed, completed in every
    /// status past the create — and the change started when the pipeline was created.
    ///
    /// The rows are the whole `PipelineStatus` enum (smithy_query: shape PipelineStatus:
    /// CREATING, ACTIVE, UPDATING, DELETING, CREATE_FAILED, UPDATE_FAILED, STARTING,
    /// START_FAILED, STOPPING, STOPPED), so a status the catalog carries that this reading
    /// does not place cannot slip through, including the ones the issue's own case never
    /// reaches.
    #[test]
    fn the_reported_progress_is_the_pipeline_status_the_create_moved() {
        let created_at = SystemTime::UNIX_EPOCH + ::std::time::Duration::from_secs(1_700_000_000);
        let rows: &[(&str, &str)] = &[
            ("CREATING", CHANGE_IN_PROGRESS),
            ("CREATE_FAILED", CHANGE_FAILED),
            ("ACTIVE", CHANGE_COMPLETED),
            ("UPDATING", CHANGE_COMPLETED),
            ("UPDATE_FAILED", CHANGE_COMPLETED),
            ("DELETING", CHANGE_COMPLETED),
            ("STARTING", CHANGE_COMPLETED),
            ("START_FAILED", CHANGE_COMPLETED),
            ("STOPPING", CHANGE_COMPLETED),
            ("STOPPED", CHANGE_COMPLETED),
        ];
        for (pipeline_status, expected) in rows {
            let reported = create_change_progress(pipeline_status, created_at);
            assert_eq!(
                reported.status.as_deref(),
                Some(*expected),
                "case {pipeline_status:?}: expected {expected:?}, got {:?}",
                reported.status
            );
            assert_eq!(
                reported.start_time,
                Some(::aws_smithy_types::DateTime::from(created_at)),
                "case {pipeline_status:?}: the change started when the pipeline was created, \
                 got {:?}",
                reported.start_time
            );
            assert_eq!(
                reported.total_number_of_stages, 0,
                "case {pipeline_status:?}: no stage is reported, got {}",
                reported.total_number_of_stages
            );
            assert_eq!(
                reported.change_progress_stages.as_deref().map(<[_]>::len),
                Some(0),
                "case {pipeline_status:?}: no stage is reported, got {:?}",
                reported.change_progress_stages
            );
        }
    }
}
