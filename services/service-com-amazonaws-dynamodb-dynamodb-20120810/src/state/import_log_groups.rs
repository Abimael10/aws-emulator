// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs side of a table import: the log group an import task reports what it
//! could not import to.
//!
//! "The Amazon Resource Number (ARN) of the Cloudwatch Log Group associated with this
//! import task" (`ImportSummary$CloudWatchLogGroupArn` /
//! `ImportTableDescription$CloudWatchLogGroupArn`) is where the per-row errors of an import
//! are reported — "Please check CloudWatch error logs for more details"
//! ([`super::IMPORT_FAILURE_ITEM_VALIDATION_MESSAGE`]) — so an import that names one names
//! a log group that EXISTS: it is created where CloudWatch Logs holds every log group
//! ([`::service_com_amazonaws_cloudwatchlogs_logs_20140328::state`]) on this same engine,
//! which is where `DescribeLogGroups` then reports it, rather than reported as an ARN
//! nothing answers to.
//!
//! LOCKING: [`import_log_group`] takes the CLOUDWATCH LOGS store's lock and nothing else,
//! so a caller holding this service's own guard may call it — the one lock order this
//! emulator uses, the same way a table's Contributor Insights rules are minted
//! ([`super::contributor_insights`]). That service never reaches into this store.

use super::*;

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;

/// The log group DynamoDB reports every import task's errors in.
///
/// One group for the account's imports rather than one per task: the member is "the
/// Cloudwatch Log Group associated with this import task", and DynamoDB associates every
/// import of a Region with the same `/aws-dynamodb/imports` group, under which each task's
/// own errors are a stream of its own.
pub const IMPORT_LOG_GROUP_NAME: &str = "/aws-dynamodb/imports";

/// The `CloudWatchLogGroupArn` an import started in `region` reports, with the log group it
/// names created where CloudWatch Logs holds log groups if it is not there yet.
///
/// The ARN is the OWNING store's own value for that group
/// ([`logs::LogGroupRecord::arn`], the form carrying the trailing `:*`, which is the ARN of
/// a log group together with its streams and the form DynamoDB reports for an import), so
/// what an import announces is what `DescribeLogGroups` reports for the group rather than an
/// ARN minted beside it. A store this engine does not hold cannot be written to, so no log
/// group is claimed either: the import then reports no ARN rather than one nothing answers
/// to.
pub fn import_log_group<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    now: SystemTime,
) -> Option<String> {
    let state = logs::service_state(engine)?;
    let created_at = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(0));
    let mut store = state.lock();
    let group = store
        .log_groups
        .entry(IMPORT_LOG_GROUP_NAME.to_string())
        .or_insert_with(|| logs::LogGroupRecord::new(region, IMPORT_LOG_GROUP_NAME, created_at));
    Some(group.arn.clone())
}
