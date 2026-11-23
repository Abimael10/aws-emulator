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
//! * WHAT EVERY DESTINATION IS TOLD IS THE BATCH. The destination gets one record per
//!   discarded batch ([`batch_record`]) naming the function, what ended the batch, how many
//!   attempts it had and what the last of them answered, beside the batch itself: the stream
//!   it was read from, the shard, the sequence numbers it starts and ends at, when its first
//!   and last record arrived and how many records it held. That window is what a client
//!   replays the dropped records from — the records are still in the shard for its retention
//!   period.
//! * A BUCKET IS ALSO TOLD THE EVENT. An object has no message-size limit to drop a payload
//!   against, so for a bucket destination alone the record carries, beside that pointer, the
//!   WHOLE event the invocation was made with ([`object_record`]) — the records as the
//!   function was handed them — which is what lets a client replay the batch out of the
//!   object itself rather than out of the stream. A queue and a topic are told the pointer
//!   and nothing more.
//! * WHERE IT GOES IS WHAT THE MAPPING NAMES. `DestinationConfig$OnFailure` is "the
//!   destination configuration for failed invocations", whose `Destination` is "the Amazon
//!   Resource Name (ARN) of the destination resource", and a mapping naming none has nowhere
//!   to record a discarded batch, so nothing is written for it. HOW it goes there is the
//!   destination service's own write ([`deliver`]): a message of the record for a queue or a
//!   topic, and one OBJECT holding it for a bucket — under the prefix that destination names,
//!   at the key such an object takes ([`record_object_key_suffix`]), so one discarded batch is
//!   one object and a reader lists them under that prefix.
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

/// The member the EVENT the invocation was made with is carried under, for the one
/// destination kind that is told it ([`object_record`]).
pub const RECORD_PAYLOAD: &str = "payload";

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
    /// The mapping's own `UUID`, which the KEY of the object a batch recorded in a bucket is
    /// written under names ([`record_object_key_suffix`]).
    pub(super) uuid: &'batch str,
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
    /// The EVENT the function was invoked with for this batch — "Lambda reads items from the
    /// event source and invokes the function" — which a BUCKET destination's object carries
    /// whole ([`object_record`]).
    pub(super) event: &'batch Value,
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
    let at = SystemTime::now();
    let record = batch_record(batch, at);
    let delivered = deliver(callback, clients, destination, &record, batch, at).await;
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

/// Write `record` to the destination `destination` names, and report whether that destination
/// ACCEPTED it.
///
/// Which call is made follows from the SERVICE the destination's ARN names, because a
/// destination is a resource Lambda writes to with that service's own write. The kinds
/// `OnFailure$Destination` enumerates for a shard source are "an Amazon SNS topic, Amazon SQS
/// queue, or Amazon S3 bucket" — a message of the record for the first two, and ONE OBJECT
/// holding it for the third ([`put_record_object`]), which is the one kind told the event
/// itself as well ([`object_record`]) — and a destination of any other kind is reported as a
/// record that was not written rather than counted as written.
async fn deliver(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    destination: &str,
    record: &Value,
    batch: &FailedBatch<'_>,
    at: SystemTime,
) -> Result<(), String> {
    let Some(parsed) = ::service_common::arn::parse(destination) else {
        return Err(format!("{destination} is no resource of any service"));
    };
    // A resource is reached in the Region its OWN ARN names; an ARN that names NONE — the
    // partition-global form an Amazon S3 bucket's ARN takes — is reached where the stream
    // is. A Region the ARN does state is held to the Region LABEL grammar every model's ARN
    // pattern states for the field ([`::service_common::arn::region_label`]), because this
    // is the one part of the destination that reaches the ADDRESS of the hop the write
    // travels over: a value carrying a separator would re-point or truncate that address, so
    // it names no Region any endpoint has and the record is reported as one that was not
    // written rather than written to a Region the destination does not name.
    let region = if parsed.region.is_empty() {
        batch.run.region.as_str()
    } else {
        let Some(stated) = ::service_common::arn::region_label(parsed.region) else {
            return Err(format!(
                "{destination} names no Region any endpoint has, so nothing was written to it"
            ));
        };
        stated
    };
    match parsed.vendor {
        "sqs" => {
            send_to_queue(
                callback,
                clients,
                region,
                parsed.resource,
                &record.to_string(),
            )
            .await
        }
        "sns" => {
            publish_to_topic(callback, clients, region, destination, &record.to_string()).await
        }
        "s3" => {
            put_record_object(
                callback,
                clients,
                region,
                parsed.resource,
                &record_object_key_suffix(batch, at),
                &object_record(record, batch).to_string(),
            )
            .await
        }
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

/// The bucket an Amazon S3 destination names, and the KEY PREFIX inside it that every record
/// written to that destination sits under.
///
/// An S3 bucket's ARN has the bucket name as its whole resource field
/// (`arn:aws:s3:::<bucket>`), so anything a caller writes after a `/` is a PLACE INSIDE the
/// bucket rather than part of its name: `arn:aws:s3:::<bucket>/<prefix>` names the bucket and
/// the prefix records of it are written under, and an ARN naming the bucket alone writes them
/// at its root.
///
/// The prefix comes back ready for a key to be appended to it, with exactly one separator
/// between every segment it names and one at its end, so two spellings of the same place name
/// the same prefix and no key carries an empty segment.
fn bucket_and_prefix(resource: &str) -> (&str, String) {
    let (bucket, prefix) = resource.split_once('/').unwrap_or((resource, ""));
    let segments: Vec<&str> = prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let prefix = if segments.is_empty() {
        String::new()
    } else {
        format!("{}/", segments.join("/"))
    };
    (bucket, prefix)
}

/// The fixed path the key of every such object begins with, below the prefix the destination
/// itself names: the records a bucket destination holds are Lambda's own, so they sit in a
/// place of Lambda's own naming rather than loose among the caller's objects.
const RECORD_KEY_ROOT: &str = "aws/lambda";

/// What the key of the object recording ONE discarded batch carries BELOW the prefix its
/// destination names ([`bucket_and_prefix`]).
///
/// AWS's own naming convention for these objects, which is what a client reading them lists
/// by: `aws/lambda/<ESM-UUID>/<shardID>/YYYY/MM/DD/YYYY-MM-DDTHH.MM.SS-<uuid>`. Everything
/// that varies in it is known about the batch — the mapping that gave up on it, the shard it
/// was read from, and the instant the record was made, written once as the date path a reader
/// asks a day of records for and once as the second the record was made at — and the last
/// segment ends in an id of this object's own, so one key belongs to one write and two batches
/// discarded in the same second of the same shard are two objects rather than one overwriting
/// the other.
fn record_object_key_suffix(batch: &FailedBatch<'_>, at: SystemTime) -> String {
    let at = ::chrono::DateTime::<::chrono::Utc>::from(at);
    format!(
        "{RECORD_KEY_ROOT}/{uuid}/{shard}/{date}/{second}-{distinct}",
        uuid = batch.uuid,
        shard = batch.shard,
        date = at.format("%Y/%m/%d"),
        second = at.format("%Y-%m-%dT%H.%M.%S"),
        distinct = ::uuid::Uuid::new_v4(),
    )
}

/// The record the OBJECT of a bucket destination holds: the pointer to the batch every
/// destination is told ([`batch_record`]), plus the whole event the invocation was made with.
///
/// An object is the one destination kind with no message-size limit to drop a payload against,
/// so the record written there carries the event itself beside the pointer: the records as the
/// function was handed them, which is what lets a client replay the discarded batch out of the
/// object alone. A queue or a topic is told the pointer and nothing more.
fn object_record(record: &Value, batch: &FailedBatch<'_>) -> Value {
    let mut object = record.clone();
    if let Some(members) = object.as_object_mut() {
        members.insert(RECORD_PAYLOAD.to_string(), batch.event.clone());
    }
    object
}

/// The media type the record of a discarded batch is stored under, which is what a later read
/// of the object reports: the record is a JSON document.
const RECORD_MEDIA_TYPE: &str = "application/json";

/// `PutObject` the record as one object of the bucket the destination names, under the prefix
/// that destination states ([`bucket_and_prefix`]) and at the key this batch takes
/// ([`record_object_key_suffix`]).
///
/// The write travels over the same in-process hop every other call of this poller does, built
/// by the workspace's one construction of the Amazon S3 client an emulated service writes an
/// object through ([`::service_common::s3_client`]).
async fn put_record_object(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    region: &str,
    resource: &str,
    key_suffix: &str,
    body: &str,
) -> Result<(), String> {
    let (bucket, prefix) = bucket_and_prefix(resource);
    let key = format!("{prefix}{key_suffix}");
    clients
        .s3(callback, region)
        .put_object()
        .bucket(bucket)
        .key(&key)
        .content_type(RECORD_MEDIA_TYPE)
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            body.as_bytes().to_vec(),
        ))
        .send()
        .await
        .map_err(|error| {
            format!(
                "s3:PutObject of {key} was not accepted ({})",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })
        .map(|_stored| ())
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
            let event = json!({ "Records": [] });
            let batch = FailedBatch {
                uuid: MAPPING_UUID,
                run: &run,
                shard: "shardId-000000000000",
                records: &records,
                attempts,
                answered: last.as_ref(),
                event: &event,
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
            // The pointer every destination is told describes the batch and carries no event
            // of it; the event is the bucket destination's own addition
            // ([`a_bucket_destinations_record_carries_the_event_whole`]).
            let rendered = written.to_string();
            assert!(
                !rendered.contains("Records") && !rendered.contains(RECORD_PAYLOAD),
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

    /// The `UUID` of the mapping whose discarded batches the rows above record — a well-formed
    /// mapping id, which the key of a bucket destination's object carries.
    const MAPPING_UUID: &str = "6c9a0e3b-9f3d-4a1e-8f27-1b2c3d4e5f60";

    /// RULE: an Amazon S3 destination's ARN names the bucket in its resource field, and
    /// everything after a `/` is the key PREFIX inside that bucket records are written under.
    ///
    /// The rows differ along what the rule generalises over: whether the ARN states a prefix at
    /// all, how deep that prefix is, and how its separators are spelled.
    #[test]
    fn a_bucket_destination_names_a_bucket_and_the_prefix_inside_it() {
        // (case, the ARN's resource field, the bucket, the prefix a key is written under)
        let rows: [(&str, &str, &str, &str); 7] = [
            ("the-bucket-alone", "failed-batches", "failed-batches", ""),
            (
                "a-one-segment-prefix",
                "failed-batches/esm",
                "failed-batches",
                "esm/",
            ),
            (
                "a-nested-prefix",
                "failed-batches/esm/orders",
                "failed-batches",
                "esm/orders/",
            ),
            (
                "a-prefix-spelled-with-a-trailing-separator",
                "failed-batches/esm/",
                "failed-batches",
                "esm/",
            ),
            (
                "a-prefix-spelled-with-doubled-separators",
                "failed-batches//esm//",
                "failed-batches",
                "esm/",
            ),
            (
                "a-nested-prefix-with-a-doubled-separator-inside-it",
                "failed-batches/esm//orders",
                "failed-batches",
                "esm/orders/",
            ),
            (
                "a-bucket-written-with-a-trailing-separator",
                "failed-batches/",
                "failed-batches",
                "",
            ),
        ];
        for (case, resource, bucket, prefix) in rows {
            let (named, under) = bucket_and_prefix(resource);
            assert_eq!(
                (named, under.as_str()),
                (bucket, prefix),
                "case {case:?}: expected {bucket:?} under {prefix:?}, got {named:?} under \
                 {under:?}"
            );
        }
    }

    /// RULE: the key of the object a bucket destination takes for ONE discarded batch follows
    /// the published naming convention for these records —
    /// `aws/lambda/<ESM-UUID>/<shardID>/YYYY/MM/DD/YYYY-MM-DDTHH.MM.SS-<uuid>` below whatever
    /// prefix the destination itself states — so a reader lists one mapping's records under
    /// `aws/lambda/<UUID>/`, one shard's below that and one day's below the date path, and no
    /// two writes land on one key.
    ///
    /// The rows differ along what the rule generalises over: which shard the batch was read
    /// from, how many records it held and the instant the record was made at.
    #[test]
    fn the_key_of_a_recorded_batch_follows_the_published_naming_convention() {
        let run = run(ShardSource::Kinesis);
        let first = record("49590338271490256608559692538361571095921575989136588898", None);
        let second = record("49590338271490256608559692538361571095921575989136588899", None);
        let one = [first.clone()];
        let both = [first.clone(), second.clone()];
        let event = json!({ "Records": [] });
        // (case, the batch, the instant the record was made, the date path and the second the
        // key carries for it)
        let rows: [(&str, FailedBatch<'_>, &str, &str, &str); 4] = [
            (
                "a-batch-of-one-record",
                discarded(&run, "shardId-000000000000", &one, &event),
                "2026-10-26T07:44:05Z",
                "2026/10/26",
                "2026-10-26T07.44.05",
            ),
            (
                "a-batch-spanning-a-window",
                discarded(&run, "shardId-000000000000", &both, &event),
                "2026-10-26T07:44:05Z",
                "2026/10/26",
                "2026-10-26T07.44.05",
            ),
            (
                "another-shard-of-the-same-mapping",
                discarded(&run, "shardId-000000000001", &one, &event),
                "2026-10-26T07:44:05Z",
                "2026/10/26",
                "2026-10-26T07.44.05",
            ),
            (
                "a-record-made-on-another-day",
                discarded(&run, "shardId-000000000000", &one, &event),
                "2027-01-01T23:59:59Z",
                "2027/01/01",
                "2027-01-01T23.59.59",
            ),
        ];
        let mut keys: Vec<String> = Vec::new();
        for (case, batch, instant, date, second) in rows {
            let at: SystemTime = ::chrono::DateTime::parse_from_rfc3339(instant)
                .expect("the fixture instant is a well-formed timestamp")
                .into();
            let expected = format!(
                "{RECORD_KEY_ROOT}/{MAPPING_UUID}/{}/{date}/{second}-",
                batch.shard
            );
            let key = record_object_key_suffix(&batch, at);
            assert!(
                key.starts_with(&expected),
                "case {case:?}: expected a key under {expected:?}, got {key:?}"
            );
            let distinct = key.strip_prefix(&expected).unwrap_or_default();
            assert!(
                ::uuid::Uuid::parse_str(distinct).is_ok(),
                "case {case:?}: the key must end in an id of the object's own, got {distinct:?} \
                 in {key:?}"
            );
            // One key belongs to one write: the same batch recorded twice takes two objects
            // rather than one overwriting the other.
            let again = record_object_key_suffix(&batch, at);
            assert_ne!(
                again, key,
                "case {case:?}: two writes must not share a key, got {again:?} twice"
            );
            for written in [key, again] {
                assert!(
                    !keys.contains(&written),
                    "case {case:?}: every write must take its own key, but {written:?} is \
                     already taken among {keys:?}"
                );
                keys.push(written);
            }
        }
    }

    /// RULE: the record written to a BUCKET destination carries the WHOLE event the invocation
    /// was made with, beside the pointer to the batch every destination is told; the record a
    /// queue or a topic is sent carries that pointer alone.
    ///
    /// The rows differ along what the rule generalises over: the shape of the event the
    /// invocation was made with.
    #[test]
    fn a_bucket_destinations_record_carries_the_event_whole() {
        let at: SystemTime = ::chrono::DateTime::parse_from_rfc3339("2026-10-26T07:44:05Z")
            .expect("the fixture instant is a well-formed timestamp")
            .into();
        let run = run(ShardSource::Kinesis);
        let records = [record(
            "49590338271490256608559692538361571095921575989136588898",
            Some(at),
        )];
        // (case, the event the invocation was made with)
        let rows: [(&str, Value); 3] = [
            (
                "an-event-of-one-record",
                json!({
                    "Records": [{
                        "eventSource": "aws:kinesis",
                        "kinesis": {
                            "sequenceNumber": records[0].sequence_number,
                            "data": "b3JkZXI=",
                        },
                    }],
                }),
            ),
            (
                "an-event-of-several-records",
                json!({
                    "Records": [
                        { "kinesis": { "sequenceNumber": "1", "data": "b25l" } },
                        { "kinesis": { "sequenceNumber": "2", "data": "dHdv" } },
                    ],
                }),
            ),
            (
                "an-event-carrying-a-tumbling-window-beside-its-records",
                json!({
                    "Records": [{ "kinesis": { "sequenceNumber": "1", "data": "b25l" } }],
                    "window": { "start": "2026-10-26T07:44:00Z" },
                    "isFinalInvokeForWindow": false,
                }),
            ),
        ];
        for (case, event) in rows {
            let batch = discarded(&run, "shardId-000000000000", &records, &event);
            let pointer = batch_record(&batch, at);
            let object = object_record(&pointer, &batch);
            assert_eq!(
                object[RECORD_PAYLOAD], event,
                "case {case:?}: the object must carry the event whole, got {object}"
            );
            for member in [
                "version",
                "timestamp",
                "requestContext",
                KINESIS_BATCH_INFO,
            ] {
                assert_eq!(
                    object[member], pointer[member],
                    "case {case:?}: the object must still say what every destination is told \
                     about {member}, got {object}"
                );
            }
            assert_eq!(
                pointer.get(RECORD_PAYLOAD),
                None,
                "case {case:?}: the record a queue or a topic is sent carries no event, got \
                 {pointer}"
            );
        }
    }

    /// One batch the mapping `run` names gave up on: the `records` it handed over, read from
    /// `shard`, with `event` as what the invocation was made with and the attempts and the
    /// answer left at one row's worth.
    fn discarded<'batch>(
        run: &'batch ShardSourcedMapping,
        shard: &'batch str,
        records: &'batch [ShardRecord],
        event: &'batch Value,
    ) -> FailedBatch<'batch> {
        FailedBatch {
            uuid: MAPPING_UUID,
            run,
            shard,
            records,
            attempts: 1,
            answered: None,
            event,
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
            batching_window: Duration::ZERO,
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
