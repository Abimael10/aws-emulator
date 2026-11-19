// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs GROUP and STREAM a job's container writes to, materialised where
//! CloudWatch Logs holds them.
//!
//! # The rule
//!
//! SageMaker runs a job's container on the account's behalf and publishes that container's
//! output to CloudWatch Logs: a training job's containers write to the group
//! [`TRAINING_JOB_LOG_GROUP`], one stream per container of the job's cluster, named for the
//! job and the container that wrote it ([`training_job_log_stream_name`]) — the layout AWS
//! documents for monitoring a training job's logs. So the group and the stream are
//! CloudWatch Logs' OWN resources from the moment the container starts: a client that lists
//! the streams of a job that has trained finds them, reads them, attaches a retention policy
//! to the group or deletes it, and gets `ResourceNotFoundException` only for a group nobody
//! published.
//!
//! Naming a stream and HOLDING it are two different things, and this module is the second:
//! the group and the stream are created in CloudWatch Logs' own store, and the records go in
//! through that service's own ingest path, over this emulator's in-process callback — so what
//! a client reads back with `DescribeLogStreams`, `GetLogEvents` or `DeleteLogGroup` is what
//! the log service holds rather than a second copy kept here. Nothing here deletes a group or
//! a stream: the records in it belong to the account.
//!
//! What a stream HOLDS is only what the container really wrote: a log event is never invented
//! for one. This emulator starts no algorithm image ([`super::container_run`]), and what it
//! keeps of a DECLARED container program is that program's exit status, its standard-error
//! tail and the files it left rather than the lines it printed, so a training job's stream is
//! opened empty — the stream of a container whose output nothing recorded reads exactly like
//! the stream of a container that printed nothing. A hook whose script's lines ARE recorded
//! publishes those lines ([`super::notebook_hook_logs`]).
//!
//! # Best effort, never fatal
//!
//! The group and the records go through the workspace's ONE vended-logs write
//! (`::service_common::cloudwatch_logs`), which every service delivering records on an
//! account's behalf uses; only the opening of a stream a container has written nothing to yet
//! is this module's own ([`opened_stream`]), because that write delivers records and creates
//! nothing for an empty batch. All of it is BEST EFFORT: an emulator with no CloudWatch Logs
//! mounted beside SageMaker, or a write CloudWatch Logs refuses, is reported in this service's
//! own tracing rather than failing the job, whose own work succeeded or failed on its own
//! terms.

use super::*;

/// The log group SageMaker publishes every training job's container logs in, as AWS
/// documents for monitoring training jobs with CloudWatch Logs.
///
/// One group for the account's training jobs, with a stream per container
/// ([`training_job_log_stream_name`]) rather than a group per job — so deleting the group is
/// an account-wide act and a teardown that keeps it deletes the job's streams instead.
pub const TRAINING_JOB_LOG_GROUP: &str = "/aws/sagemaker/TrainingJobs";

/// The container of a training job's cluster that RAN: the job's first algorithm container,
/// which is the one whose work this emulator performs ([`super::run_training_job`] fits the
/// model once, in process).
///
/// AWS names a training job's containers `algo-1` upwards, one per ML compute instance of the
/// cluster; a stream is published for the container that did the work and for no instance
/// nothing ran on.
const FIRST_ALGORITHM_CONTAINER: &str = "algo-1";

/// The stream a training job's first algorithm container writes to: the job's own name, the
/// container, and the second the container started at — the layout AWS documents for a
/// training job's log streams (`[training-job-name]/algo-[container]-[epoch timestamp]`).
#[must_use]
pub fn training_job_log_stream_name(job_name: &str, started_at: SystemTime) -> String {
    format!(
        "{job_name}/{FIRST_ALGORITHM_CONTAINER}-{}",
        epoch_seconds(started_at)
    )
}

/// Open the log stream a training job's algorithm container writes to, in the group
/// SageMaker publishes training-job logs in.
///
/// Called where the job's algorithm container STARTS, so a run that never got that far — one
/// whose input data could not be downloaded — publishes no stream for a container that never
/// ran.
///
/// The stream is opened with NO event in it, and this publication delivers none: a training
/// job's log records are the lines its algorithm container printed, and the record of what a
/// container printed is not one this emulator holds — it starts no algorithm image, and the
/// session that performs a DECLARED container program keeps that program's exit status, its
/// standard-error tail and the files it left rather than the stream of lines it wrote
/// ([`super::ContainerOutcome`]). So nothing is written here instead of them. Delivering a
/// job's container output is the job of the change that makes the run keep it, through
/// [`published_container_logs`], which this crate's hook publication already delivers over.
///
/// `true` once CloudWatch Logs holds the group and the stream. Best effort — see this
/// module's header. Call it with NO store guard held: it awaits.
pub async fn publish_training_job_logs(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    job_name: &str,
    started_at: SystemTime,
) -> bool {
    let log_stream = training_job_log_stream_name(job_name, started_at);
    published_container_logs(ctx, region, TRAINING_JOB_LOG_GROUP, &log_stream, &[]).await
}

/// Materialise `log_group` and `log_stream` where CloudWatch Logs does not hold them yet, and
/// deliver one log event per entry of `records` into the stream.
///
/// The ONE publication path of this crate's job logs, so a training job's stream and a
/// notebook instance's hook stream are brought into existence the same way: the group, which
/// SageMaker publishes under its own name, then the stream, then the records — and an empty
/// `records` leaves the stream open and empty rather than unopened.
///
/// `true` once CloudWatch Logs holds the group and the stream, whether or not there was
/// anything to write into it. Call it with NO store guard held: it awaits.
pub async fn published_container_logs(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_group: &str,
    log_stream: &str,
    records: &[String],
) -> bool {
    // The group is one this service PUBLISHES rather than one it was pointed at, so it exists
    // from the moment a container starts writing to it and before any record is in it.
    if !::service_common::cloudwatch_logs::provisioned(ctx, region, log_group).await {
        return false;
    }
    if !opened_stream(ctx, region, log_group, log_stream).await {
        return false;
    }
    if records.is_empty() {
        return true;
    }
    // The records go in through CloudWatch Logs' own ingest path, inside the bounds
    // `PutLogEvents` publishes for one batch, by the one vended-logs write this workspace has.
    ::service_common::cloudwatch_logs::delivered(ctx, region, log_group, log_stream, records).await
}

/// Open `log_stream` in `log_group`, where CloudWatch Logs does not hold it yet.
///
/// The one piece of the publication the shared vended-logs write does not cover: that write
/// delivers RECORDS, so it creates nothing at all for a container that has written none, and a
/// container's stream exists from the moment the container starts.
///
/// `true` once CloudWatch Logs holds the stream — including a stream it already held, which is
/// the one this container has been writing to.
async fn opened_stream(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_group: &str,
    log_stream: &str,
) -> bool {
    match cloudwatch_logs_client(ctx, region)
        .create_log_stream()
        .log_group_name(log_group)
        .log_stream_name(log_stream)
        .send()
        .await
    {
        Ok(_) => true,
        // The stream this container already published under: a later write appends to it
        // rather than opening a second one.
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_resource_already_exists_exception()) =>
        {
            true
        }
        Err(error) => {
            // The names are rendered in their escaped `Debug` form, so a value a caller had
            // a hand in cannot forge a log line.
            ::tracing::warn!(
                log_group = ?log_group,
                log_stream = ?log_stream,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the log stream this job's container writes to could not be opened, so \
                 nothing can read or delete it"
            );
            false
        }
    }
}

/// `at` in whole seconds since the Unix epoch, which is the stamp a log stream's name
/// carries ([`training_job_log_stream_name`]).
///
/// An instant before the epoch, which no container start can be, clamps to the epoch rather
/// than wrapping.
fn epoch_seconds(at: SystemTime) -> u64 {
    at.duration_since(::std::time::SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// A CloudWatch Logs client over the in-process callback into this same emulator.
///
/// The same seam, and the same reasoning, as this service's Amazon S3 client
/// ([`super::s3_access::s3_client`]): the callback routes in process, so the `endpoint_url`
/// is an ADDRESS the in-process router reads rather than a host any resolver looks up, no
/// socket is opened and this client reaches the emulator's own CloudWatch Logs and nothing
/// else. There is nothing to retry, time out or cache an identity for, each of which would
/// need a sleep implementation this client is not given.
fn cloudwatch_logs_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_cloudwatchlogs::Client {
    let region = ::service_common::arn::region_label(region).unwrap_or(DEFAULT_REGION);
    let config = ::aws_sdk_cloudwatchlogs::Config::builder()
        .behavior_version(::aws_sdk_cloudwatchlogs::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudwatchlogs::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://logs.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_cloudwatchlogs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudwatchlogs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudwatchlogs::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudwatchlogs::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudwatchlogs::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stream a training job's container writes to is the job's name, the container and
    /// the second it started at — one row per shape of job name and start instant.
    #[test]
    fn a_container_stream_is_named_for_its_job_and_start() {
        let epoch = ::std::time::SystemTime::UNIX_EPOCH;
        let cases = [
            ("xgboost-2026-01-01-00-00-00-000", 0_u64),
            ("a", 1_762_000_000),
            ("Aa0123456789-0123456789", 59),
        ];
        for (job, seconds) in cases {
            let started_at = epoch + Duration::from_secs(seconds);
            let actual = training_job_log_stream_name(job, started_at);
            let expected = format!("{job}/algo-1-{seconds}");
            assert_eq!(
                actual, expected,
                "case {job:?} at {seconds}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// An instant before the epoch — which no container start is — stamps the epoch rather
    /// than wrapping.
    #[test]
    fn an_instant_before_the_epoch_stamps_the_epoch() {
        let before = ::std::time::SystemTime::UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(
            epoch_seconds(before),
            0,
            "an instant before the epoch: expected 0, got {}",
            epoch_seconds(before)
        );
    }
}
