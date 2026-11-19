// SPDX-License-Identifier: CC0-1.0

//! What a pipe does with a batch it GAVE UP ON: where the record of it goes, and what that
//! record says.
//!
//! A stream source's parameters carry two members about giving up — "discard records after
//! the specified number of retries" (`MaximumRetryAttempts`) and "define the target queue to
//! send dead-letter queue events to" (`DeadLetterConfig`) — so a batch a pipe could not get
//! through its stages is attempted while those retries last and then DISCARDED, and the
//! destination the second member names is the one place a client can still see that it
//! existed. Everything about when that happens is [`super`]'s (the attempts are counted
//! there, against the batch the source stands at); this module is the DESTINATION and the
//! MESSAGE.
//!
//! The message is a POINTER, not the data: it names the pipe, the stage that failed, and the
//! shard and the run of sequence numbers the discarded batch covered, so a client can read
//! exactly those records off the stream again — the records' own payloads are never copied
//! into it.
//!
//! WHICH destinations a pipe may name is also this module's: "either an Amazon SNS topic or
//! Amazon SQS queue ARN" ([`resolve`]), and a value naming anything else is a destination no
//! pipe may dead-letter to — which is why the lifecycle asks the same question before it
//! brings a pipe up at all ([`super::super::bring_up`]).

use std::time::SystemTime;

use ::serde_json::{json, Value};

use super::{Clients, FailedAttempt, LiveRecord, RunningPipe, SEND_MESSAGE_ACTION};

/// The IAM action a dead letter written to an Amazon SNS topic is authorized under: the
/// action name of the SNS API call that takes it.
pub(super) const PUBLISH_ACTION: &str = "sns:Publish";

/// The version the message's own envelope reports: the layout below, which is the one every
/// dead letter this emulator writes carries.
const PAYLOAD_VERSION: &str = "1.0";

/// What the message reports the batch was discarded FOR: the retries its source parameters
/// allowed are spent ("discard records after the specified number of retries",
/// `PipeSourceKinesisStreamParameters$MaximumRetryAttempts`).
const RETRIES_EXHAUSTED: &str = "RetryAttemptsExhausted";

/// The member a discarded KINESIS batch is pointed at by, and the member a discarded
/// DynamoDB-stream batch is: a dead letter says which kind of stream its pointer addresses,
/// so one source's batch is never described as the other's.
pub(super) const KINESIS_BATCH_INFO: &str = "KinesisBatchInfo";
/// See [`KINESIS_BATCH_INFO`].
pub(super) const DYNAMODB_BATCH_INFO: &str = "DDBStreamBatchInfo";

/// The ARN service namespaces a dead-letter destination may be named in: "for Amazon
/// Kinesis stream and Amazon DynamoDB stream sources, specify either an Amazon SNS topic or
/// Amazon SQS queue ARN" (`DeadLetterConfig$Arn`). A value in any other namespace names no
/// destination a pipe may dead-letter to.
const SQS_NAMESPACE: &str = "sqs";
/// See [`SQS_NAMESPACE`].
const SNS_NAMESPACE: &str = "sns";

/// Where a pipe's dead letters go: the resource its `DeadLetterConfig.Arn` names.
///
/// "For Amazon Kinesis stream and Amazon DynamoDB stream sources, specify either an Amazon
/// SNS topic or Amazon SQS queue ARN" (`DeadLetterConfig$Arn`), so those two are the kinds
/// addressed here — a queue the record is SENT to, and a topic it is PUBLISHED to. A
/// destination in any other service namespace is one no pipe may name ([`resolve`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Destination {
    /// An Amazon SQS queue: the record travels as one message on it.
    Queue {
        /// The queue's ARN as the pipe configured it: the resource the write is authorized
        /// on.
        arn: String,
        /// The url Amazon SQS takes that queue's `SendMessage` at.
        url: String,
        /// The Region the ARN names — a dead letter is written in the destination's own
        /// Region.
        region: String,
    },
    /// An Amazon SNS topic: the record travels as one message published to it, which its
    /// own subscriptions then carry.
    Topic {
        /// The topic's ARN as the pipe configured it, which is both the resource the write
        /// is authorized on and what `Publish` addresses.
        arn: String,
        /// See [`Self::Queue::region`].
        region: String,
    },
}

impl Destination {
    /// The ARN the pipe named this destination by.
    pub(super) fn arn(&self) -> &str {
        match self {
            Self::Queue { arn, .. } | Self::Topic { arn, .. } => arn,
        }
    }
}

/// Why `arn` names no dead-letter destination a pipe may configure.
///
/// The fixed text is the rule AWS states for the member itself — "For Amazon Kinesis stream
/// and Amazon DynamoDB stream sources, specify either an Amazon SNS topic or Amazon SQS queue
/// ARN" (`DeadLetterConfig$Arn`, `smithy_query(query="doc", shape="DeadLetterConfig",
/// member="Arn")`) — and the one variable part is the destination the configuration names,
/// read from the pipe's own record. One sentence for every value the member cannot carry, so
/// the lifecycle's bring-up decision ([`super::super::bring_up`]) and this data plane's own
/// reading of a pipe's configuration report such a value identically.
fn unsupported_destination(arn: &str) -> String {
    format!(
        "DeadLetterConfig.Arn {arn} is not valid. For Amazon Kinesis stream and Amazon \
         DynamoDB stream sources, specify either an Amazon SNS topic or Amazon SQS queue ARN."
    )
}

/// The destination the `DeadLetterConfig.Arn` `arn` names, or why it names none a pipe may
/// dead-letter to.
pub(super) fn resolve(arn: &str) -> Result<Destination, String> {
    let Some(parsed) = ::service_common::arn::parse(arn) else {
        return Err(unsupported_destination(arn));
    };
    // Neither a queue nor a topic carries a resource type or a path: a value whose resource
    // field is compound names something else of that service's — a subscription, a platform
    // endpoint — and not the destination itself.
    let name = parsed.resource;
    let compound = name.contains('/') || name.contains(':');
    match parsed.vendor {
        SQS_NAMESPACE if !compound => Ok(Destination::Queue {
            arn: arn.to_string(),
            url: super::queue_url(parsed.region, parsed.account, name),
            region: parsed.region.to_string(),
        }),
        SNS_NAMESPACE if !compound => Ok(Destination::Topic {
            arn: arn.to_string(),
            region: parsed.region.to_string(),
        }),
        _ => Err(unsupported_destination(arn)),
    }
}

/// Why the `DeadLetterConfig.Arn` `arn` cannot be a pipe's dead-letter destination, or
/// `None` when it names one.
pub(super) fn refusal(arn: &str) -> Option<String> {
    resolve(arn).err()
}

/// The batch a pipe discarded, as a dead letter POINTS AT it: which shard it was read from
/// and the run of sequence numbers it covered.
#[derive(Debug)]
pub(super) struct DiscardedBatch {
    /// The member this pointer is reported under, by the kind of stream it was read from
    /// ([`KINESIS_BATCH_INFO`]).
    pub(super) member: &'static str,
    /// The shard the batch was read from: one position of the stream, which is what makes
    /// the sequence numbers below an addressable range.
    pub(super) shard: String,
    /// The sequence number of the batch's FIRST record and of its LAST: together they bound
    /// exactly the records that were discarded, so a client can read that run again.
    pub(super) start_sequence_number: String,
    /// See [`Self::start_sequence_number`].
    pub(super) end_sequence_number: String,
    /// When the stream reports the first record of the batch arrived, and the last — absent
    /// for a record that carries no instant.
    pub(super) first_arrival: Option<String>,
    /// See [`Self::first_arrival`].
    pub(super) last_arrival: Option<String>,
    /// How many records the discarded batch held.
    pub(super) size: usize,
}

/// The message the pipe `run` sends to its dead-letter destination for the batch it
/// discarded after `attempts` failed attempts at it.
///
/// The failure CONTEXT and nothing else: the pipe that gave up, the stage that failed, why it
/// gave up and how many attempts it took, the error the failed stage's own resource named
/// when it named one, and the pointer bounding the records. None of the records' own payloads
/// travels in it.
pub(super) fn payload(
    run: &RunningPipe,
    batch: &DiscardedBatch,
    failure: &FailedAttempt,
    attempts: u32,
) -> Value {
    let mut pointer = json!({
        "shardId": batch.shard,
        "startSequenceNumber": batch.start_sequence_number,
        "endSequenceNumber": batch.end_sequence_number,
        "batchSize": batch.size,
        "streamArn": run.source_arn,
    });
    if let Some(arrived_at) = &batch.first_arrival {
        pointer["approximateArrivalOfFirstRecord"] = json!(arrived_at);
    }
    if let Some(arrived_at) = &batch.last_arrival {
        pointer["approximateArrivalOfLastRecord"] = json!(arrived_at);
    }
    let mut message = json!({
        "timestamp": instant_text(&::aws_smithy_types::DateTime::from(SystemTime::now()))
            .unwrap_or_default(),
        "version": PAYLOAD_VERSION,
        "context": {
            "pipeArn": run.pipe_arn,
            "stage": failure.stage.reported_stage(),
            "condition": RETRIES_EXHAUSTED,
            "approximateInvokeCount": attempts,
        },
    });
    // What the resource the failed stage called said about its own run, when it said
    // anything: a function that raised reports it on `FunctionError`, which is the one
    // description of the failure that is the invoked resource's own words.
    if let Some(error) = &failure.function_error {
        message["responseContext"] = json!({ "functionError": error });
    }
    message[batch.member] = pointer;
    message
}

/// Send `body` to the pipe's dead-letter destination AS THE PIPE'S EXECUTION ROLE, or why
/// it could not be sent.
///
/// The same question every other stage's call is preceded by is asked here too: a role that
/// may not write to the destination is a dead letter that does not happen, and the caller
/// keeps the batch rather than discarding a record of it nothing holds. WHICH call is made
/// and which action it is authorized under follow from the kind of destination the pipe
/// named: `SendMessage` on a queue, `Publish` on a topic.
pub(super) async fn send(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    record: &LiveRecord<'_>,
    destination: &Destination,
    body: &Value,
) -> Result<(), String> {
    match destination {
        Destination::Queue { arn, url, region } => {
            if let Some(detail) = record.refusal(SEND_MESSAGE_ACTION, arn) {
                return Err(detail);
            }
            clients
                .sqs(callback, region)
                .send_message()
                .queue_url(url)
                .message_body(body.to_string())
                .send()
                .await
                .map_err(|error| not_taken(arn, &error))?;
        }
        Destination::Topic { arn, region } => {
            if let Some(detail) = record.refusal(PUBLISH_ACTION, arn) {
                return Err(detail);
            }
            clients
                .sns(callback, region)
                .publish()
                .topic_arn(arn)
                .message(body.to_string())
                .send()
                .await
                .map_err(|error| not_taken(arn, &error))?;
        }
    }
    Ok(())
}

/// What a destination that did not take the record of a discarded batch is reported with:
/// the destination and what its own service answered.
fn not_taken<E: ::std::error::Error>(arn: &str, error: &E) -> String {
    format!(
        "the dead-letter destination {arn} did not take the record of the discarded batch \
         ({})",
        ::aws_smithy_types::error::display::DisplayErrorContext(error)
    )
}

/// The instant `at` as the text a dead letter reports a time in, or `None` for one that
/// cannot be rendered.
pub(super) fn instant_text(at: &::aws_smithy_types::DateTime) -> Option<String> {
    at.fmt(::aws_smithy_types::date_time::Format::DateTime).ok()
}

#[cfg(test)]
mod tests {
    use super::super::{test_support::kinesis_run, CallFailure, FailedAttempt, Stage};
    use super::{
        payload, refusal, resolve, DiscardedBatch, DYNAMODB_BATCH_INFO, KINESIS_BATCH_INFO,
    };

    /// One discarded batch of two records read from one shard.
    fn discarded(member: &'static str) -> DiscardedBatch {
        DiscardedBatch {
            member,
            shard: "shardId-000000000000".to_string(),
            start_sequence_number: "49590338271490256608559692538361571095921575989136588898"
                .to_string(),
            end_sequence_number: "49590338271490256608559692538361571095921575989136588899"
                .to_string(),
            first_arrival: Some("2026-01-01T00:00:00Z".to_string()),
            last_arrival: Some("2026-01-01T00:00:01Z".to_string()),
            size: 2,
        }
    }

    /// RULE: a dead letter POINTS AT the discarded batch — the pipe that gave up, why it
    /// gave up, the error the failed stage's resource named when it named one, and the shard
    /// and run of sequence numbers the batch covered, under the member its SOURCE KIND is
    /// reported under — and carries none of the records' own payloads.
    ///
    /// The rows differ along the kind of stream the batch came from, the stage that failed
    /// and whether that stage's resource named an error of its own, which is what the rule
    /// generalises over.
    #[test]
    fn a_dead_letter_points_at_the_discarded_batch() {
        // (case, the member the pointer travels under, the attempt that failed)
        let rows: [(&str, &str, FailedAttempt); 4] = [
            (
                "a kinesis batch the enrichment could not be invoked for",
                KINESIS_BATCH_INFO,
                FailedAttempt::withheld(Stage::Enrichment, "the stage failed".to_string()),
            ),
            (
                "a kinesis batch the target failed",
                KINESIS_BATCH_INFO,
                FailedAttempt::withheld(Stage::Target, "the stage failed".to_string()),
            ),
            (
                "a kinesis batch whose enrichment raised",
                KINESIS_BATCH_INFO,
                FailedAttempt::from_call(
                    Stage::Enrichment,
                    CallFailure {
                        detail: "the enrichment answered an Unhandled error".to_string(),
                        function_error: Some("Unhandled".to_string()),
                        refusal: None,
                    },
                ),
            ),
            (
                "a dynamodb-stream batch the enrichment could not be invoked for",
                DYNAMODB_BATCH_INFO,
                FailedAttempt::withheld(Stage::Enrichment, "the stage failed".to_string()),
            ),
        ];
        for (case, member, failure) in rows {
            let run = kinesis_run("TRIM_HORIZON");
            let batch = discarded(member);
            let named = failure.function_error.clone();
            let message = payload(&run, &batch, &failure, 3);
            let pointer = &message[member];
            assert_eq!(
                pointer["shardId"],
                ::serde_json::json!(batch.shard),
                "case {case:?}: expected the shard the batch was read from, got {message}"
            );
            assert_eq!(
                pointer["startSequenceNumber"],
                ::serde_json::json!(batch.start_sequence_number),
                "case {case:?}: expected the batch's first sequence number, got {message}"
            );
            assert_eq!(
                pointer["endSequenceNumber"],
                ::serde_json::json!(batch.end_sequence_number),
                "case {case:?}: expected the batch's last sequence number, got {message}"
            );
            assert_eq!(
                pointer["batchSize"],
                ::serde_json::json!(2),
                "case {case:?}: expected the discarded batch's size, got {message}"
            );
            assert_eq!(
                pointer["streamArn"],
                ::serde_json::json!(run.source_arn),
                "case {case:?}: expected the stream it was read off, got {message}"
            );
            assert_eq!(
                message["context"]["pipeArn"],
                ::serde_json::json!(run.pipe_arn),
                "case {case:?}: expected the pipe that gave up, got {message}"
            );
            assert_eq!(
                message["context"]["condition"],
                ::serde_json::json!("RetryAttemptsExhausted"),
                "case {case:?}: expected the retries to be reported as spent, got {message}"
            );
            assert_eq!(
                message["context"]["approximateInvokeCount"],
                ::serde_json::json!(3),
                "case {case:?}: expected the attempts the batch cost, got {message}"
            );
            assert_eq!(
                message["context"]["stage"],
                ::serde_json::json!(failure.stage.reported_stage()),
                "case {case:?}: expected the stage that gave up, got {message}"
            );
            assert_eq!(
                message["version"],
                ::serde_json::json!("1.0"),
                "case {case:?}: expected the envelope's version, got {message}"
            );
            assert_eq!(
                message["responseContext"]["functionError"].as_str(),
                named.as_deref(),
                "case {case:?}: expected the invoked resource's own error, got {message}"
            );
            // The one member the other batch-info member must not be reported under.
            let other = if member == KINESIS_BATCH_INFO {
                DYNAMODB_BATCH_INFO
            } else {
                KINESIS_BATCH_INFO
            };
            assert!(
                message.get(other).is_none(),
                "case {case:?}: a batch is pointed at under its own source's member only, \
                 got {message}"
            );
        }
    }

    /// RULE: a dead-letter destination is one of the two kinds `DeadLetterConfig.Arn`
    /// admits — "either an Amazon SNS topic or Amazon SQS queue ARN" — and a value naming
    /// anything else is refused rather than taken for one.
    ///
    /// The rows differ along what the configured ARN names, which is what the rule
    /// generalises over.
    #[test]
    fn only_a_queue_or_topic_destination_is_written_to() {
        // (case, the configured ARN, whether it names a destination)
        let rows: [(&str, &str, bool); 9] = [
            (
                "a queue",
                "arn:aws:sqs:us-east-1:000000000000:dead-letters",
                true,
            ),
            (
                "a queue in another account",
                "arn:aws:sqs:eu-west-1:111111111111:dead-letters",
                true,
            ),
            (
                "an sns topic",
                "arn:aws:sns:us-east-1:000000000000:dead-letters",
                true,
            ),
            (
                "an sns topic in another partition",
                "arn:aws-us-gov:sns:us-gov-west-1:000000000000:dead-letters",
                true,
            ),
            (
                "an sns subscription rather than the topic",
                "arn:aws:sns:us-east-1:000000000000:dead-letters:8f3a",
                false,
            ),
            (
                "a kinesis stream",
                "arn:aws:kinesis:us-east-1:000000000000:stream/dead-letters",
                false,
            ),
            (
                "a lambda function",
                "arn:aws:lambda:us-east-1:000000000000:function:dead-letters",
                false,
            ),
            (
                "an eventbridge event bus",
                "arn:aws:events:us-east-1:000000000000:event-bus/dead-letters",
                false,
            ),
            ("not an ARN at all", "dead-letters", false),
        ];
        for (case, arn, served) in rows {
            let resolved = resolve(arn);
            assert_eq!(
                resolved.is_ok(),
                served,
                "case {case:?}: expected served={served}, got {resolved:?}"
            );
            assert_eq!(
                refusal(arn).is_none(),
                served,
                "case {case:?}: the refusal and the resolution have to agree, got {:?}",
                refusal(arn)
            );
            if let Ok(destination) = resolved {
                assert_eq!(
                    destination.arn(),
                    arn,
                    "case {case:?}: a destination is named by the ARN the pipe configured"
                );
            }
        }
    }
}
