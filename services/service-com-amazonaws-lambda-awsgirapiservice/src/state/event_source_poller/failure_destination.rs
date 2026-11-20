// SPDX-License-Identifier: CC0-1.0

//! What becomes of a batch a mapping on a SHARD source gives up on: what the record of that
//! batch says, and where it goes.
//!
//! `MaximumRetryAttempts` is "Discard records after the specified number of retries. The
//! default value is infinite (-1). When set to infinite (-1), failed records are retried
//! until the record expires", so the count a mapping states bounds the attempts ONE batch
//! gets ([`super::attempts_allowed`]) and a mapping that states none keeps attempting it. The
//! discard itself — reading on past the batch — is [`super::failed_attempt`]'s; this module is
//! the other half of it, the only thing a client can still learn about the records that were
//! dropped:
//!
//! * WHAT IS RECORDED IS THE BATCH, NOT ITS PAYLOADS. The destination gets one record per
//!   discarded batch ([`batch_record`]) naming the function, what ended the batch, how many
//!   attempts it had and what the last of them answered, beside the batch itself: the stream
//!   it was read from, the shard, the sequence numbers it starts and ends at, when its first
//!   and last record arrived and how many records it held. That window is what a client
//!   replays the dropped records from, which a copy of the payloads would not give it — the
//!   records are still in the shard for its retention period.
//! * WHERE IT GOES IS WHAT THE MAPPING NAMES. `DestinationConfig$OnFailure` is "the
//!   destination configuration for failed invocations", whose `Destination` is "the Amazon
//!   Resource Name (ARN) of the destination resource", and a mapping naming none has nowhere
//!   to record a discarded batch, so nothing is written for it.
//! * A HEALTHY BATCH RECORDS NOTHING. The record exists for the batch Lambda gave up on, so
//!   a batch that was processed — or one that is still being attempted — writes nothing to
//!   the destination at all.
//!
//! The destination is a caller-chosen value and nothing of it is DIALLED: it is an ARN, and
//! the only things read out of it are the service it belongs to and the Region it names,
//! which decide which service OF THIS EMULATOR is asked and in which Region. No host name is
//! built from it, no name is resolved and no socket is opened — the write travels over the
//! same in-process callback as every other call this poller makes ([`super::Clients`]) — so a
//! destination this emulator does not serve itself is a record that was not delivered, and
//! is reported as one.

use ::std::time::SystemTime;

use ::serde_json::{json, Value};

use super::{Answered, Clients, ShardRecord, ShardSourcedMapping};
use crate::ops::types::DestinationConfig;
use crate::state::ShardSource;

/// The schema version of the record one discarded batch is written as.
pub const RECORD_SCHEMA_VERSION: &str = "1.0";

/// What `requestContext.condition` reports for a batch that had every attempt the mapping's
/// `MaximumRetryAttempts` allows it: the retries are what ran out.
pub const CONDITION_RETRY_ATTEMPTS_EXHAUSTED: &str = "RetryAttemptsExhausted";

/// The member the batch of an Amazon Kinesis data stream is described under.
pub const KINESIS_BATCH_INFO: &str = "KinesisBatchInfo";

/// The member the batch of an Amazon DynamoDB table's stream is described under.
pub const DYNAMODB_STREAM_BATCH_INFO: &str = "DDBStreamBatchInfo";

/// The destination a mapping's `DestinationConfig$OnFailure` names, or `None` when it names
/// none.
///
/// `DestinationArn`'s own pattern is `^$|kafka://…|arn:…`, so the EMPTY STRING is a value the
/// member admits and the only one that is no resource at all: it is how a caller takes a
/// destination away, and it names no destination rather than an ARN of nothing — the one
/// reading of the member this crate has ([`crate::state::event_invoke_config`]).
#[must_use]
pub fn on_failure_destination(destination_config: Option<&DestinationConfig>) -> Option<String> {
    crate::state::event_invoke_config::named_destination(
        destination_config?
            .on_failure
            .as_ref()?
            .destination
            .as_deref(),
    )
}

/// One batch a mapping has given up on: everything the record written for it is built from.
pub(super) struct FailedBatch<'batch> {
    /// The mapping that read the batch, which names the function, the stream and the
    /// destination.
    pub(super) run: &'batch ShardSourcedMapping,
    /// The shard the batch was read from.
    pub(super) shard: &'batch str,
    /// The records the batch held, in the order the shard holds them.
    pub(super) records: &'batch [ShardRecord],
    /// How many attempts the batch had.
    pub(super) attempts: u32,
    /// What the LAST attempt's invocation answered, and `None` when that attempt could not
    /// invoke the function at all.
    pub(super) answered: Option<&'batch Answered>,
}

/// Write the record of the batch `batch` names to the destination its mapping configures, and
/// report what that write came to.
///
/// A mapping naming no destination has nowhere to record the batch, so nothing is written and
/// nothing is reported: the discard itself is the caller's, and it happens either way.
///
/// The write is made ACTING AS the invoked function's execution role, so it is subject to the
/// policies in force on that role: a destination whose write they withhold receives nothing at
/// all ([`withheld_write`]). Nothing is retried and the discard stands either way — the
/// records are gone from the function's reach whether or not the record of them landed.
pub(super) async fn discard_batch(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    policies: &super::PolicyStores,
    batch: &FailedBatch<'_>,
) {
    let Some(destination) = batch.run.on_failure_destination.as_deref() else {
        return;
    };
    if let Some(withheld) = withheld_write(policies, batch.run, destination) {
        ::tracing::warn!(
            function = %batch.run.function_arn,
            stream = %batch.run.stream_arn,
            shard = %batch.shard,
            destination = %destination,
            detail = %withheld,
            "the record of a discarded batch was not written: the policies in force on the \
             function's execution role withhold the write its failure destination takes"
        );
        return;
    }
    let record = batch_record(batch, SystemTime::now());
    let delivered = deliver(
        callback,
        clients,
        &batch.run.region,
        destination,
        &record.to_string(),
    )
    .await;
    if let Err(detail) = delivered {
        // The write is not part of what any caller was told — no client asked for this batch
        // to be read — so a failure is reported here and nothing is retried.
        ::tracing::warn!(
            function = %batch.run.function_arn,
            stream = %batch.run.stream_arn,
            shard = %batch.shard,
            destination = %destination,
            detail = %detail,
            "the record of a discarded batch did not reach the mapping's failure destination"
        );
    }
}

/// Why the write that would record this batch at `destination` is WITHHELD — from the
/// execution role of the function the mapping invokes — or `None` when it is not.
///
/// "Lambda reads items from the event source and invokes the function"
/// (`CreateEventSourceMapping`) acting as that function's execution role, and the record of a
/// batch it gave up on is one more call Lambda makes on the customer's behalf under the same
/// identity, so it is decided by the same documents the poller's READS of the source are
/// ([`super::withheld_shard_read`]) — and on the same evidence, so an engine carrying no IAM
/// store, a role that store holds no record for, a mapping whose role this store cannot read
/// and a destination kind no write is stated for all withhold nothing. The decision itself is
/// [`crate::state::event_invoke_config::withheld_destination_write`], the one reading of what a
/// role may write to a destination this crate has; the sentence is for the one log line a
/// withheld write writes, since no caller is waiting on this write to be told anything.
fn withheld_write(
    policies: &super::PolicyStores,
    run: &ShardSourcedMapping,
    destination: &str,
) -> Option<String> {
    let role_arn = run.invoke_identity_arn.as_deref()?;
    let iam = policies.iam.as_ref()?;
    let (action, service) = crate::state::event_invoke_config::withheld_destination_write(
        &iam.lock(),
        role_arn,
        destination,
    )?;
    Some(format!(
        "{role_arn} may not call {action} on {service} for {destination}"
    ))
}

/// The record ONE discarded batch is written as.
///
/// The request context is the invocation that last attempted the batch — its own request id,
/// the function it was made against, what ended the batch and how many attempts it had — and
/// the response context is what that attempt answered. An attempt that could not invoke the
/// function at all answers nothing, so no response context is reported for it rather than one
/// the record would be inventing. The batch is described under the member its own source is
/// described under ([`batch_info_member`]).
pub(super) fn batch_record(batch: &FailedBatch<'_>, at: SystemTime) -> Value {
    let mut request_context = json!({
        "functionArn": batch.run.function_arn,
        "condition": CONDITION_RETRY_ATTEMPTS_EXHAUSTED,
        "approximateInvokeCount": batch.attempts,
    });
    let mut record = json!({
        "version": RECORD_SCHEMA_VERSION,
        "timestamp": crate::state::iso8601(at),
    });
    if let Some(answered) = batch.answered {
        if let (Some(request_id), Some(context)) =
            (answered.request_id.as_deref(), request_context.as_object_mut())
        {
            context.insert("requestId".to_string(), json!(request_id));
        }
        let mut response_context = json!({});
        if let Some(context) = response_context.as_object_mut() {
            if let Some(status_code) = answered.status_code {
                context.insert("statusCode".to_string(), json!(status_code));
            }
            if let Some(executed_version) = answered.executed_version.as_deref() {
                context.insert("executedVersion".to_string(), json!(executed_version));
            }
            if let Some(function_error) = answered.function_error.as_deref() {
                context.insert("functionError".to_string(), json!(function_error));
            }
        }
        if let Some(object) = record.as_object_mut() {
            object.insert("responseContext".to_string(), response_context);
        }
    }
    if let Some(object) = record.as_object_mut() {
        object.insert("requestContext".to_string(), request_context);
        object.insert(
            batch_info_member(batch.run.source).to_string(),
            batch_info(batch),
        );
    }
    record
}

/// Which member the batch of `source` is described under: each shard source describes its
/// batch under a member of its own.
fn batch_info_member(source: ShardSource) -> &'static str {
    match source {
        ShardSource::Kinesis => KINESIS_BATCH_INFO,
        ShardSource::DynamoDbStream => DYNAMODB_STREAM_BATCH_INFO,
    }
}

/// The BATCH itself: the stream and shard it was read from, the window of sequence numbers it
/// covers, when its first and last record arrived, and how many records it held.
///
/// The window is what makes the discarded records replayable, so it is the batch's own first
/// and last record — which for a batch of one record is that record on both ends. An arrival
/// instant is reported only where the source stamped one.
fn batch_info(batch: &FailedBatch<'_>) -> Value {
    let first = batch.records.first();
    let last = batch.records.last();
    let mut info = json!({
        "shardId": batch.shard,
        "batchSize": batch.records.len(),
        "streamArn": batch.run.stream_arn,
    });
    let object = info.as_object_mut();
    let Some(object) = object else {
        return info;
    };
    if let Some(first) = first {
        object.insert(
            "startSequenceNumber".to_string(),
            json!(first.sequence_number),
        );
        if let Some(arrived_at) = stamped(first) {
            object.insert("approximateArrivalOfFirstRecord".to_string(), json!(arrived_at));
        }
    }
    if let Some(last) = last {
        object.insert("endSequenceNumber".to_string(), json!(last.sequence_number));
        if let Some(arrived_at) = stamped(last) {
            object.insert("approximateArrivalOfLastRecord".to_string(), json!(arrived_at));
        }
    }
    info
}

/// When the source stamped `record`, as the record reports an instant, or `None` for a record
/// its source stamped no instant on.
fn stamped(record: &ShardRecord) -> Option<String> {
    let arrived_at = record.arrived_at?;
    SystemTime::try_from(arrived_at)
        .ok()
        .map(crate::state::iso8601)
}

/// Write `body` to the destination `destination` names, and report whether that destination
/// ACCEPTED it.
///
/// Which call is made follows from the SERVICE the destination's ARN names, because a
/// destination is a resource Lambda writes to with that service's own write. The kinds
/// `OnFailure$Destination` enumerates for a shard source are "an Amazon SNS topic, Amazon SQS
/// queue, or Amazon S3 bucket"; the two whose write is one message of the record travel here,
/// and a destination of any other kind is reported as a record that was not written rather
/// than counted as written.
async fn deliver(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    stream_region: &str,
    destination: &str,
    body: &str,
) -> Result<(), String> {
    let Some(parsed) = ::service_common::arn::parse(destination) else {
        return Err(format!("{destination} is no resource of any service"));
    };
    // A resource is reached in the Region its OWN ARN names; an ARN that names none is
    // reached where the stream is.
    let region = if parsed.region.is_empty() {
        stream_region
    } else {
        parsed.region
    };
    match parsed.vendor {
        "sqs" => send_to_queue(callback, clients, region, parsed.resource, body).await,
        "sns" => publish_to_topic(callback, clients, region, destination, body).await,
        other => Err(format!(
            "this emulator writes no record to {other} for a failure destination, so nothing \
             was written to {destination}"
        )),
    }
}

/// `SendMessage` the record to the queue the destination names.
///
/// A queue is addressed by its `QueueUrl`, which is the url the Amazon SQS emulator of this
/// same engine publishes for the queue that name belongs to
/// ([`::service_com_amazonaws_sqs_amazonsqs::state::queue_url`]) — the ONE spelling of it, so
/// the address written to is the address a client of that queue holds.
async fn send_to_queue(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    region: &str,
    queue: &str,
    body: &str,
) -> Result<(), String> {
    let url = ::service_com_amazonaws_sqs_amazonsqs::state::queue_url(region, queue);
    clients
        .sqs(callback, region)
        .send_message()
        .queue_url(url)
        .message_body(body)
        .send()
        .await
        .map_err(|error| {
            format!(
                "sqs:SendMessage was not accepted ({})",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })
        .map(|_accepted| ())
}

/// `Publish` the record to the topic the destination names.
async fn publish_to_topic(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    region: &str,
    topic: &str,
    body: &str,
) -> Result<(), String> {
    clients
        .sns(callback, region)
        .publish()
        .topic_arn(topic)
        .message(body)
        .send()
        .await
        .map_err(|error| {
            format!(
                "sns:Publish was not accepted ({})",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })
        .map(|_accepted| ())
}

#[cfg(test)]
mod tests {
    use ::std::collections::BTreeMap;
    use ::std::time::Duration;

    use super::*;
    use crate::ops::types::{OnFailure, OnSuccess};
    use crate::state::event_source_poller::MAX_RECORDS_PER_BATCH;
    use crate::state::ShardCursor;

    /// RULE: the destination a discarded batch is recorded at is the one
    /// `DestinationConfig$OnFailure` names, and the empty string the member's pattern admits
    /// names none.
    #[test]
    fn the_failure_destination_is_the_one_the_on_failure_slot_names() {
        let queue = "arn:aws:sqs:us-east-1:000000000000:failed-batches";
        let topic = "arn:aws:sns:us-east-1:000000000000:failed-batches";
        // (case, the configuration, the destination a discarded batch is recorded at)
        let rows: [(&str, Option<DestinationConfig>, Option<&str>); 5] = [
            ("no-configuration", None, None),
            (
                "a-configuration-naming-no-slot",
                Some(destination_config(None, None)),
                None,
            ),
            (
                "an-on-failure-queue",
                Some(destination_config(None, Some(queue))),
                Some(queue),
            ),
            (
                "an-on-failure-topic-beside-an-on-success-slot",
                Some(destination_config(Some(queue), Some(topic))),
                Some(topic),
            ),
            (
                "an-on-failure-slot-cleared-with-the-empty-destination",
                Some(destination_config(None, Some(""))),
                None,
            ),
        ];
        for (case, config, expected) in rows {
            let named = on_failure_destination(config.as_ref());
            assert_eq!(
                named.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
    }

    /// One row of the record table: the case, the source the batch was read from, the
    /// records it held, how many attempts it had and what the last of them answered.
    struct Row {
        case: &'static str,
        source: ShardSource,
        records: Vec<ShardRecord>,
        attempts: u32,
        last: Option<Answered>,
    }

    /// RULE: the record of a discarded batch describes the BATCH — the stream, the shard, the
    /// sequence-number window, the arrival instants and the record count — under the member
    /// its own source is described under, beside the function, the condition and the attempt
    /// count; it carries no record payload.
    ///
    /// The rows differ along what the rule generalises over: which shard source the batch was
    /// read from, how many records it held, whether its records carry an arrival instant and
    /// what the last attempt answered.
    #[test]
    fn the_record_of_a_discarded_batch_describes_the_batch() {
        let arrived_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let rows: Vec<Row> = vec![
            Row {
                case: "a-kinesis-batch-of-one-record",
                source: ShardSource::Kinesis,
                records: vec![record(
                    "49590338271490256608559692538361571095921575989136588898",
                    Some(arrived_at),
                )],
                attempts: 1,
                last: Some(answered(Some("Unhandled"))),
            },
            Row {
                case: "a-kinesis-batch-of-several-records",
                source: ShardSource::Kinesis,
                records: vec![
                    record(
                        "49590338271490256608559692538361571095921575989136588898",
                        Some(arrived_at),
                    ),
                    record(
                        "49590338271490256608559692538361571095921575989136588899",
                        Some(arrived_at + Duration::from_secs(2)),
                    ),
                ],
                attempts: 3,
                last: Some(answered(Some("Unhandled"))),
            },
            Row {
                case: "a-table-stream-batch",
                source: ShardSource::DynamoDbStream,
                records: vec![record("800000000003126276949", Some(arrived_at))],
                attempts: 2,
                last: Some(answered(None)),
            },
            Row {
                case: "a-batch-whose-records-carry-no-arrival-instant",
                source: ShardSource::DynamoDbStream,
                records: vec![record("800000000003126276950", None)],
                attempts: 2,
                last: None,
            },
        ];
        for row in rows {
            let Row {
                case,
                source,
                records,
                attempts,
                last,
            } = row;
            let run = run(source);
            let batch = FailedBatch {
                run: &run,
                shard: "shardId-000000000000",
                records: &records,
                attempts,
                answered: last.as_ref(),
            };
            let written = batch_record(&batch, arrived_at + Duration::from_secs(10));
            let member = match source {
                ShardSource::Kinesis => KINESIS_BATCH_INFO,
                ShardSource::DynamoDbStream => DYNAMODB_STREAM_BATCH_INFO,
            };
            let info = &written[member];
            assert_eq!(
                info["streamArn"],
                json!(run.stream_arn),
                "case {case:?}: the batch must name the stream it was read from, got {written}"
            );
            assert_eq!(
                info["shardId"],
                json!("shardId-000000000000"),
                "case {case:?}: the batch must name its shard, got {written}"
            );
            assert_eq!(
                info["batchSize"],
                json!(records.len()),
                "case {case:?}: the batch must report how many records it held, got {written}"
            );
            assert_eq!(
                info["startSequenceNumber"],
                json!(records[0].sequence_number),
                "case {case:?}: the window must start at the batch's first record, got {written}"
            );
            assert_eq!(
                info["endSequenceNumber"],
                json!(records[records.len() - 1].sequence_number),
                "case {case:?}: the window must end at the batch's last record, got {written}"
            );
            assert_eq!(
                written["requestContext"]["condition"],
                json!(CONDITION_RETRY_ATTEMPTS_EXHAUSTED),
                "case {case:?}: the record must say what ended the batch, got {written}"
            );
            assert_eq!(
                written["requestContext"]["approximateInvokeCount"],
                json!(attempts),
                "case {case:?}: the record must report the attempts the batch had, got {written}"
            );
            assert_eq!(
                written["requestContext"]["functionArn"],
                json!(run.function_arn),
                "case {case:?}: the record must name the function, got {written}"
            );
            assert_eq!(
                written["version"],
                json!(RECORD_SCHEMA_VERSION),
                "case {case:?}: the record must carry its schema version, got {written}"
            );
            let stamped = records.iter().all(|record| record.arrived_at.is_some());
            assert_eq!(
                info.get("approximateArrivalOfFirstRecord").is_some(),
                stamped,
                "case {case:?}: an arrival instant is reported exactly where the source \
                 stamped one, got {written}"
            );
            // The record describes the batch and carries no payload of it.
            let rendered = written.to_string();
            assert!(
                !rendered.contains("Records") && !rendered.contains("payload"),
                "case {case:?}: the record must carry no record payload, got {rendered}"
            );
            match last.as_ref() {
                Some(last) => {
                    assert_eq!(
                        written["responseContext"]["statusCode"],
                        json!(last.status_code),
                        "case {case:?}: the record must report what the attempt answered, got \
                         {written}"
                    );
                    assert_eq!(
                        written["responseContext"]["functionError"],
                        last.function_error
                            .as_deref()
                            .map_or(Value::Null, |error| json!(error)),
                        "case {case:?}: the record must report the attempt's function error, \
                         got {written}"
                    );
                }
                None => assert_eq!(
                    written.get("responseContext"),
                    None,
                    "case {case:?}: an attempt that answered nothing reports no response \
                     context, got {written}"
                ),
            }
        }
    }

    /// A `DestinationConfig` stating the two slots, each with the destination given.
    fn destination_config(
        on_success: Option<&str>,
        on_failure: Option<&str>,
    ) -> DestinationConfig {
        DestinationConfig {
            on_success: on_success.map(|destination| OnSuccess {
                destination: Some(destination.to_string()),
            }),
            on_failure: on_failure.map(|destination| OnFailure {
                destination: Some(destination.to_string()),
            }),
        }
    }

    /// One record of a batch, by the sequence number the shard names it by. What the record is
    /// REPORTED with is the stamp its source published, which is what this module writes; the
    /// write instant it is also stamped with decides its age and nothing here.
    fn record(sequence_number: &str, arrived_at: Option<SystemTime>) -> ShardRecord {
        ShardRecord {
            sequence_number: sequence_number.to_string(),
            arrived_at: arrived_at.map(::aws_smithy_types::DateTime::from),
            written_at: arrived_at.map(::aws_smithy_types::DateTime::from),
            envelope: json!({ "eventID": sequence_number }),
        }
    }

    /// What an attempt that ran the handler answered, with `function_error` as its report.
    fn answered(function_error: Option<&str>) -> Answered {
        Answered {
            status_code: Some(200),
            executed_version: Some("$LATEST".to_string()),
            function_error: function_error.map(str::to_string),
            request_id: Some("8f7b1f2e-0a3c-4f6d-9c2b-5d1e7a9c3b11".to_string()),
            payload: None,
        }
    }

    /// The mapping the records above were read by, on `source`.
    fn run(source: ShardSource) -> ShardSourcedMapping {
        let stream_arn = match source {
            ShardSource::Kinesis => "arn:aws:kinesis:us-east-1:000000000000:stream/orders",
            ShardSource::DynamoDbStream => {
                "arn:aws:dynamodb:us-east-1:000000000000:table/orders/stream/2026-01-01T00:00:00.000"
            }
        };
        ShardSourcedMapping {
            source,
            stream_arn: stream_arn.to_string(),
            region: "us-east-1".to_string(),
            function_arn: "arn:aws:lambda:us-east-1:000000000000:function:consume".to_string(),
            function_name: "consume".to_string(),
            invoke_identity_arn: None,
            batch_size: MAX_RECORDS_PER_BATCH,
            in_flight_batches: 1,
            filters: Vec::new(),
            reports_batch_item_failures: false,
            attempts_allowed: Some(2),
            bisects_on_function_error: false,
            on_failure_destination: Some(
                "arn:aws:sqs:us-east-1:000000000000:failed-batches".to_string(),
            ),
            maximum_record_age: None,
            tumbling_window: None,
            meters_event_counts: false,
            cursors: BTreeMap::new(),
            initial_cursor: ShardCursor::TrimHorizon,
        }
    }
}
