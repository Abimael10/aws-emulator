// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs stream a PLACED job's container writes to, MATERIALISED where
//! CloudWatch Logs holds it.
//!
//! # The rule
//!
//! A Batch job that has been placed as a task has a CloudWatch Logs log stream:
//! `AttemptContainerDetail$logStreamName` is "the name of the CloudWatch Logs log stream
//! that's associated with the container. The log group for Batch jobs is `/aws/batch/job`",
//! and the `awslogs` driver the job's `logConfiguration` selects — the default for a
//! container job — "specifies the Amazon CloudWatch Logs logging driver"
//! (`LogConfiguration$logDriver`). So the stream a placement NAMES
//! ([`job_log_stream_name`]) has to EXIST in CloudWatch Logs, in the log group that
//! configuration delivers to: a client that lists, reads or DELETES the stream of a job
//! that has run finds it there, and a delete of it is performed rather than refused.
//!
//! Naming the stream and holding it are two different things, and this module is the
//! second: the name is decided at the placement (`placement.rs`), and the stream it names
//! is created in CloudWatch Logs' own store, through this emulator's in-process callback —
//! so what a client reads back with `DescribeLogStreams`, `GetLogEvents` or
//! `DeleteLogStream` is what the log service holds, never a second copy of it kept here.
//! The stream is created EMPTY: a container nothing ran wrote no output, so no log event
//! is invented for it (`vendor/engine/src/lib.rs`).
//!
//! # Where it is delivered
//!
//! The driver's own options decide, exactly as they do for an Amazon ECS task's container
//! (the sibling `awslogs` module in the ECS crate): [`OPTION_LOG_GROUP`] names the log
//! group, defaulting to Batch's own [`DEFAULT_JOB_LOG_GROUP`]; [`OPTION_LOG_REGION`] names
//! the Region whose log store holds it, defaulting to the Region of the job itself; and
//! [`OPTION_CREATE_GROUP`] is what has the driver create the log group as well as the
//! stream — a group nobody created and nothing was asked to create takes the delivery
//! nowhere. A configuration whose driver is NOT `awslogs` delivers the container's output
//! to that other driver's destination, so there is no CloudWatch Logs stream for it at all
//! and none is reported.
//!
//! # How it reaches the log service
//!
//! The creation is queued under this store's lock, where the placement happens, and drained
//! by the caller once that lock is released ([`ComAmazonawsBatchAwsbatchv20160810State::take_pending_log_streams`],
//! [`open_pending_log_streams`]) — the same shape the job state changes this service
//! publishes follow ([`publish_job_state_changes`]), and for the same reason: no in-process
//! request is made while a guard is held. The queue is a ring bounded by
//! [`MAX_PENDING_LOG_STREAMS`], so a run that submits jobs and never calls again cannot
//! grow the heap, and one drain opens at most [`MAX_OPENED_PER_DRAIN`] streams, so one call
//! cannot multiply into an unbounded number of in-process requests.

use crate::ops::types::LogConfiguration;

use super::*;

/// The log group a Batch job's container logs go to when its configuration names none:
/// "the log group for Batch jobs is `/aws/batch/job`"
/// (`AttemptContainerDetail$logStreamName`).
pub const DEFAULT_JOB_LOG_GROUP: &str = "/aws/batch/job";

/// The log driver that "specifies the Amazon CloudWatch Logs logging driver"
/// (`LogConfiguration$logDriver`), and so the only one whose output a CloudWatch Logs
/// stream holds. It is also the driver a container job gets when it configures none.
pub const LOG_DRIVER_AWSLOGS: &str = "awslogs";

/// The `logConfiguration` option naming the log group the driver writes to.
const OPTION_LOG_GROUP: &str = "awslogs-group";

/// The option naming the Region whose log store holds that group; the job's own Region when
/// the configuration names none.
const OPTION_LOG_REGION: &str = "awslogs-region";

/// The option naming the PREFIX of the stream the driver writes a container's output to:
/// "`awslogs-stream-prefix` ... the `awslogs-stream-prefix` option allows you to associate a
/// log stream with the specified prefix, the container name, and the ID of the Amazon ECS task
/// that the container belongs to" (`LogConfiguration$options`, the `awslogs` driver's
/// documentation).
const OPTION_STREAM_PREFIX: &str = "awslogs-stream-prefix";

/// The option that has the driver create the log group as well as the stream.
const OPTION_CREATE_GROUP: &str = "awslogs-create-group";

/// The value an `awslogs` boolean option is TRUE at: the driver reads its options as the
/// strings a job definition carries, and `"true"` is the one that turns one on.
const OPTION_TRUE: &str = "true";

/// The longest Region name the [`OPTION_LOG_REGION`] option is read as naming one. An AWS
/// Region name is a short, lower-case, hyphenated code (`us-west-2`), and this bounds what a
/// job definition's option can put in front of the delivery.
const MAX_REGION_NAME_CHARS: usize = 32;

/// How many stream creations this store holds unopened.
///
/// This store's OWN bound, not a limit AWS publishes. A placement queues one creation and
/// a job is placed at most once, so the queue cannot hold more than the jobs store itself
/// ([`limits::MAX_JOBS`]); it is a ring, so the oldest unopened creation is dropped before
/// a new one is queued rather than letting the queue grow.
const MAX_PENDING_LOG_STREAMS: usize = limits::MAX_JOBS;

/// How many streams ONE drain opens, under [`MAX_PUBLISHED_PER_DRAIN`]'s rule: what a drain
/// leaves behind keeps the clock pump awake
/// ([`ComAmazonawsBatchAwsbatchv20160810State::next_job_due_or_release_pump`]), so nothing
/// is dropped — it is opened a tick later.
const MAX_OPENED_PER_DRAIN: usize = 50;

/// Whether `value` is spelled as an AWS Region name: between one and
/// [`MAX_REGION_NAME_CHARS`] characters, each of them a lower-case letter, a digit or a
/// hyphen.
fn is_region_name(value: &str) -> bool {
    let length = value.chars().count();
    (1..=MAX_REGION_NAME_CHARS).contains(&length)
        && value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

/// ONE log stream a placed job's container writes to: where it is, whether its group is
/// the driver's to create, and the events the driver delivers to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLogStream {
    /// The Region whose CloudWatch Logs store holds the group ([`OPTION_LOG_REGION`]).
    pub region: String,
    /// The log group the stream lives in ([`OPTION_LOG_GROUP`], or
    /// [`DEFAULT_JOB_LOG_GROUP`]).
    pub log_group: String,
    /// The stream itself, as the placement named it ([`job_log_stream_name`]).
    pub log_stream: String,
    /// Whether the log GROUP is created along with the stream: Batch's own default group is
    /// the service's to hold, and a group the configuration names is created only when it
    /// asked for that ([`OPTION_CREATE_GROUP`]).
    pub create_group: bool,
    /// The events the driver delivers to the stream, as `(timestamp, message)` in the order
    /// the container wrote them.
    ///
    /// EMPTY for the record a placement queues: the stream is opened for the TASK, before
    /// any line exists, and the lines the container's program wrote are queued as their own
    /// record once that program has run ([`super::container_run`]) — so a container that
    /// wrote nothing leaves exactly the one empty stream behind.
    pub events: Vec<(i64, String)>,
}

/// The PREFIX the `awslogs` driver names a job container's log stream with: the
/// [`OPTION_STREAM_PREFIX`] option the container's configuration carries, and the name of the
/// job definition the job resolved to when it carries none — the default AWS Batch's own log
/// stream naming uses (`AttemptContainerDetail$logStreamName`).
///
/// An option present but EMPTY names no prefix, so the default stands rather than a stream
/// name beginning with a separator.
pub fn job_log_stream_prefix(
    configuration: Option<&LogConfiguration>,
    job_definition_reference: &str,
) -> String {
    configuration
        .filter(|configuration| configuration.log_driver == LOG_DRIVER_AWSLOGS)
        .and_then(|configuration| configuration.options.as_ref())
        .and_then(|options| options.get(OPTION_STREAM_PREFIX))
        .map(String::as_str)
        .filter(|prefix| !prefix.is_empty())
        .map_or_else(
            || job_definition_reference_name(job_definition_reference).to_string(),
            str::to_string,
        )
}

/// Where the container `configuration` describes delivers the stream `log_stream`, or
/// `None` when it delivers no CloudWatch Logs stream at all.
///
/// A configuration naming another driver delivers its container's output to that driver,
/// and an [`OPTION_LOG_GROUP`] or [`OPTION_LOG_REGION`] value that names no group or Region
/// CloudWatch Logs could hold names no destination either — in neither case is a different
/// group quietly put in its place. A container job that configures NO driver gets the
/// `awslogs` default, which is the case the recorded Batch log stream is.
pub fn job_log_target(
    configuration: Option<&LogConfiguration>,
    log_stream: &str,
    placement_region: &str,
) -> Option<JobLogStream> {
    let options = match configuration {
        Some(configuration) if configuration.log_driver != LOG_DRIVER_AWSLOGS => return None,
        Some(configuration) => configuration.options.clone().unwrap_or_default(),
        None => HashMap::new(),
    };
    let log_group = match options.get(OPTION_LOG_GROUP) {
        Some(named) if ::service_common::cloudwatch_logs::is_log_group_name(named) => named.clone(),
        Some(_) => return None,
        None => DEFAULT_JOB_LOG_GROUP.to_string(),
    };
    let region = match options.get(OPTION_LOG_REGION) {
        Some(named) if is_region_name(named) => named.clone(),
        Some(_) => return None,
        None => placement_region.to_string(),
    };
    // The group Batch itself holds for job logs is created with the stream; any other one
    // is the caller's, and the driver creates it only when the option says so.
    let create_group = log_group == DEFAULT_JOB_LOG_GROUP
        || options
            .get(OPTION_CREATE_GROUP)
            .is_some_and(|value| value == OPTION_TRUE);
    Some(JobLogStream {
        region,
        log_group,
        log_stream: log_stream.to_string(),
        create_group,
        events: Vec::new(),
    })
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// Queue the log stream a placement just named, for creation in CloudWatch Logs.
    ///
    /// Called from the ONE place a placement happens
    /// ([`ComAmazonawsBatchAwsbatchv20160810State::place_job`]), so a stream is queued once
    /// per attempt and never once per read. A ring bounded by [`MAX_PENDING_LOG_STREAMS`].
    pub(super) fn queue_log_stream(&mut self, stream: JobLogStream) {
        while self.pending_log_streams.len() >= MAX_PENDING_LOG_STREAMS {
            self.pending_log_streams.pop_front();
        }
        self.pending_log_streams.push_back(stream);
    }

    /// Take at most [`MAX_OPENED_PER_DRAIN`] of the streams not yet created, oldest first.
    ///
    /// Taking the records out under the lock is what stops two concurrent requests from
    /// creating the same stream twice.
    pub fn take_pending_log_streams(&mut self) -> Vec<JobLogStream> {
        let taking = MAX_OPENED_PER_DRAIN.min(self.pending_log_streams.len());
        self.pending_log_streams.drain(..taking).collect()
    }

    /// Whether this store still holds a stream nothing has created yet, which is what keeps
    /// the clock pump awake until every placed job's stream exists.
    pub(super) fn has_pending_log_streams(&self) -> bool {
        !self.pending_log_streams.is_empty()
    }
}

/// Create every log stream this store's placements have named and nothing has created yet,
/// and deliver the events the containers of those placements wrote.
///
/// Called by the publisher once this store's lock is released, so no guard is held across
/// the in-process request. Creation is BEST EFFORT, as every cross-service delivery in this
/// tree is: a log group the account has since deleted, a stream CloudWatch Logs refuses and
/// an emulator with no CloudWatch Logs mounted to ask are reported in tracing, and the
/// operation's own result stands. A stream that already exists is the stream this placement
/// named, so that refusal is not a failure.
pub(super) async fn open_pending_log_streams(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    state: &::engine::ServiceState<ComAmazonawsBatchAwsbatchv20160810State>,
) {
    // The guard is taken and dropped inside this statement: the records are moved out here
    // so nothing is created twice, and no lock is held across an await.
    let streams = state.lock().take_pending_log_streams();
    for stream in streams {
        let client = logs_client(callback, &stream.region);
        if stream.create_group {
            match client
                .create_log_group()
                .log_group_name(&stream.log_group)
                .send()
                .await
            {
                Ok(_) => {}
                Err(error)
                    if error
                        .as_service_error()
                        .is_some_and(|service| service.is_resource_already_exists_exception()) => {}
                Err(error) => {
                    ::tracing::warn!(
                        log_group = %stream.log_group,
                        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                        "a placed Batch job's log group could not be created"
                    );
                }
            }
        }
        match client
            .create_log_stream()
            .log_group_name(&stream.log_group)
            .log_stream_name(&stream.log_stream)
            .send()
            .await
        {
            Ok(_) => {}
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|service| service.is_resource_already_exists_exception()) => {}
            Err(error) => {
                ::tracing::warn!(
                    log_group = %stream.log_group,
                    log_stream = %stream.log_stream,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "a placed Batch job's log stream could not be created, so nothing can \
                     read or delete it"
                );
            }
        }
        if stream.events.is_empty() {
            continue;
        }
        // What the container WROTE, delivered to the stream the driver opened for it: the
        // lines are the program's own output ([`super::container_run`]), so the stream holds
        // what ran and nothing else. They are in the order the container wrote them, which
        // is chronological — "the log events in the batch must be in chronological order by
        // their timestamp" (`PutLogEvents`).
        let events: Vec<::aws_sdk_cloudwatchlogs::types::InputLogEvent> = stream
            .events
            .iter()
            .filter_map(|(at, message)| {
                ::aws_sdk_cloudwatchlogs::types::InputLogEvent::builder()
                    .timestamp(*at)
                    .message(message)
                    .build()
                    .ok()
            })
            .collect();
        if let Err(error) = client
            .put_log_events()
            .log_group_name(&stream.log_group)
            .log_stream_name(&stream.log_stream)
            .set_log_events(Some(events))
            .send()
            .await
        {
            ::tracing::warn!(
                log_group = %stream.log_group,
                log_stream = %stream.log_stream,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the output of a placed Batch job's container could not be delivered to its \
                 log stream"
            );
        }
    }
}

/// A CloudWatch Logs client over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// routes in process, so no socket is opened and this crate names no other service's crate.
/// The configuration is the one every callback client in this workspace uses (see the
/// EventBridge client beside it in [`publish_job_state_changes`]): nothing to retry, time
/// out or watch for a stalled stream, because the transport is in process, and the
/// credentials the callback hands over are the emulator's own.
///
/// `region` is ADDRESSING for that transport rather than a host anything connects to: the
/// driver's option names which Region's log store the group is in, and the only thing it can
/// reach is this process.
fn logs_client(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_cloudwatchlogs::Client {
    let config = ::aws_sdk_cloudwatchlogs::Config::builder()
        .behavior_version(::aws_sdk_cloudwatchlogs::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudwatchlogs::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://logs.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_cloudwatchlogs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudwatchlogs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudwatchlogs::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudwatchlogs::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudwatchlogs::Client::from_conf(config)
}
