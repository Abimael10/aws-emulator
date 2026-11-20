// SPDX-License-Identifier: CC0-1.0

//! How an event source mapping reads an Amazon KINESIS DATA STREAM: how that stream's
//! shards are listed, how one batch is taken off a shard at the position the mapping
//! carries, and what one record is handed to the function as.
//!
//! Everything a mapping does with the batch once it has it — metering its age, invoking
//! the function, checkpointing past what was processed — is the one rule for every shard
//! source and lives in [`super`]. This module is only the Kinesis half of the questions
//! that differ per source, the other half being
//! [`super::dynamodb_stream`]: how the shards are listed, what one record is handed over as,
//! and what a `FilterCriteria` pattern is matched against ([`filter_event`]).
//!
//! The stream is read through the Kinesis surface of this same emulator (`ListShards`,
//! `GetShardIterator`, `GetRecords`, `SubscribeToShard`) over the in-process callback, so a
//! record is consumed only through the data plane a client would consume it through.
//!
//! WHICH of those readers a pass uses is the mapping's event source's own: `EventSourceArn`
//! is "the ARN of the data stream or a stream consumer", so a mapping may name either, and
//! a consumer's ARN is its stream's ARN continued with `/consumer/<name>:<timestamp>`
//! ([`data_stream_arn`]). Both name the SAME shards, and the shards are listed on the data
//! stream either way, because `ListShards` addresses a stream; what differs is how one batch
//! is taken off a shard:
//!
//! * an event source naming the data stream reads SHARED throughput, the pull reader a
//!   client uses — `GetShardIterator` at the mapping's position, then `GetRecords`
//!   ([`iterated_batch`]);
//! * an event source naming a stream CONSUMER reads that consumer's ENHANCED FAN-OUT
//!   subscription, `SubscribeToShard` from the mapping's position ([`subscribed_batch`]) —
//!   "this operation establishes an HTTP/2 connection between the consumer you specify in
//!   the ConsumerARN parameter and the shard you specify in the ShardId parameter", so the
//!   records travel as the events of that subscription rather than as a page of a pull read.
//!
//! Everything the pass then does with the batch is the same for both, because what a reader
//! delivers is the shard's own records: a mapping on a consumer hands its function the same
//! records, in the same order, as one on the stream itself.

use std::borrow::Cow;

use ::serde_json::{json, Value};
use ::service_com_amazonaws_kinesis_kinesis_20131202::state as kinesis_store;

use super::{Clients, ShardRecord, ShardSourcedMapping, StreamShards, MAX_SHARD_PAGES};
use crate::state::ShardCursor;

/// The `eventSource` a Kinesis record envelope reports, and the `eventName` each record
/// carries, as the Kinesis event Lambda hands a function spells them.
const KINESIS_EVENT_SOURCE: &str = "aws:kinesis";
/// See [`KINESIS_EVENT_SOURCE`].
const KINESIS_EVENT_NAME: &str = "aws:kinesis:record";
/// The schema version of the `kinesis` member of one record envelope.
const KINESIS_SCHEMA_VERSION: &str = "1.0";
/// The version of the record envelope itself.
const KINESIS_EVENT_VERSION: &str = "1.0";

/// The most events of ONE subscription this reader takes before it closes the batch it is
/// gathering, so a subscription cannot keep the poller reading it.
///
/// An EMULATOR bound rather than an AWS number, and a backstop beside the batch's own ceiling
/// ([`super::batch_records`]): the batch is full, or the subscription's stream has ended, long
/// before this — one event of a subscription carries up to everything a single read of a shard
/// delivers — and an event carrying no record at all advances neither of those, so the count of
/// events one pass reads is bounded too. What is left behind is read again from the position
/// this batch ends at.
const MAX_SUBSCRIPTION_EVENTS: usize = 100;

/// The DATA STREAM whose shards a mapping over this source reads: the event source itself
/// where it names a stream, and the stream a stream CONSUMER reads where it names one.
///
/// `EventSourceArn` is "the ARN of the data stream or a stream consumer", and a consumer's
/// ARN is its stream's ARN continued with `/consumer/<name>:<timestamp>` — the form
/// `ConsumerARN`'s own pattern requires — so the stream a consumer addresses is everything
/// before that segment, read through the one place this repository states that rule
/// (`kinesis_store::stream_arn_of_consumer`). The shards are listed and the iterator opened
/// on THIS ARN whichever of the two the mapping named, because both calls address a stream.
fn data_stream_arn(run: &ShardSourcedMapping) -> &str {
    kinesis_store::stream_arn_of_consumer(&run.stream_arn)
}

/// The stream CONSUMER the mapping's event source names, and `None` for an event source that
/// names the data stream itself.
///
/// A consumer's ARN extends its stream's ([`data_stream_arn`]), so an event source naming one
/// is exactly an event source that is not the stream it addresses — and a mapping onto a
/// consumer is delivered its records through that consumer's own subscription
/// ([`subscribed_batch`]).
fn fan_out_consumer(run: &ShardSourcedMapping) -> Option<&str> {
    let event_source = run.stream_arn.as_str();
    (data_stream_arn(run) != event_source).then_some(event_source)
}

/// Every open and closed shard of the mapped stream, in the order the stream reports them.
///
/// A record of this source carries its own partition key, so the listing reports no
/// attribute to read one from ([`partition_key`]).
pub(super) async fn shard_ids(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    run: &ShardSourcedMapping,
) -> Result<StreamShards, String> {
    let stream_arn = data_stream_arn(run);
    let mut shards: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_SHARD_PAGES {
        let mut request = clients.kinesis(callback, &run.region).list_shards();
        // "NextToken" and the stream the first page named are mutually exclusive: a
        // continuation already carries the stream it is a continuation of.
        request = match next_token.take() {
            Some(token) => request.next_token(token),
            None => request.stream_arn(stream_arn),
        };
        let answer = request.send().await.map_err(|error| {
            format!(
                "the shards of {stream_arn} could not be listed ({})",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
        shards.extend(
            answer
                .shards()
                .iter()
                .map(|shard| shard.shard_id().to_string()),
        );
        match answer.next_token() {
            Some(token) => next_token = Some(token.to_string()),
            None => break,
        }
    }
    Ok(StreamShards {
        shards,
        partition_key_attribute: None,
    })
}

/// The PARTITION KEY of one record of this source: "Identifies which shard in the stream the
/// data record is assigned to" (`Record$PartitionKey`), which the record itself carries, so
/// grouping the records of a pass by it needs nothing of the stream's listing.
pub(super) fn partition_key(envelope: &Value) -> String {
    envelope
        .get("kinesis")
        .and_then(|record| record.get("partitionKey"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One batch off `shard` at `cursor`, each record already rendered as the envelope Lambda
/// hands a Kinesis record over as; `None` for a shard with no position left to read from.
///
/// Which reader the records come through is the mapping's event source's own — the
/// consumer's subscription for an event source naming a stream consumer, the shard iterator
/// for one naming the data stream (see the module docs) — and what a record is handed over as
/// is the same either way.
pub(super) async fn read_batch(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    run: &ShardSourcedMapping,
    shard: &str,
    cursor: &ShardCursor,
) -> Result<Option<Vec<ShardRecord>>, String> {
    let read = match fan_out_consumer(run) {
        Some(consumer_arn) => {
            Some(subscribed_batch(callback, clients, run, consumer_arn, shard, cursor).await?)
        }
        None => iterated_batch(callback, clients, run, shard, cursor).await?,
    };
    Ok(read.map(|records| {
        records
            .iter()
            .map(|record| ShardRecord {
                sequence_number: record.sequence_number().to_string(),
                arrived_at: record.approximate_arrival_timestamp().copied(),
                // "The approximate time that the record was inserted into the stream", at the
                // resolution this service stamped it: for this source the stamp a read carries
                // IS the write, so a record's age is measured from the same instant it reports.
                written_at: record.approximate_arrival_timestamp().copied(),
                envelope: record_envelope(record, shard, run),
            })
            .collect()
    }))
}

/// One batch off `shard` through the SHARED-throughput pull reader: the iterator that reads
/// the mapped stream from `cursor`, and the records it answers. `None` for a shard that has no
/// position left to read from.
async fn iterated_batch(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    run: &ShardSourcedMapping,
    shard: &str,
    cursor: &ShardCursor,
) -> Result<Option<Vec<::aws_sdk_kinesis::types::Record>>, String> {
    let Some(iterator) = shard_iterator(callback, clients, run, shard, cursor).await? else {
        return Ok(None);
    };
    records_at(callback, clients, run, &iterator).await.map(Some)
}

/// One batch off `shard` through the ENHANCED FAN-OUT subscription of the consumer
/// `consumer_arn` names: `SubscribeToShard` from the position the mapping carries, and the
/// records the events of that subscription deliver.
///
/// "After the connection is successfully established, Kinesis Data Streams pushes records
/// from the shard to the consumer over this connection" (`SubscribeToShard`), so the records
/// are read off the events of the stream that call answers, in the order they arrive, until
/// the subscription's stream ends or the batch is full. One batch holds at most what the
/// mapping's `BatchSize` admits ([`super::batch_records`]), which is the count the pull
/// reader asks its page for — a subscription is not asked for a count, so the batch is cut to
/// it here and the records past it are read again from the position this batch ends at.
///
/// An in-band exception frame of the subscription — the stream's own CMK no longer usable,
/// say — is an event the reader cannot deliver and is reported as a failed pass: nothing is
/// handed over and no cursor moves, so the records stay in the shard.
async fn subscribed_batch(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    run: &ShardSourcedMapping,
    consumer_arn: &str,
    shard: &str,
    cursor: &ShardCursor,
) -> Result<Vec<::aws_sdk_kinesis::types::Record>, String> {
    let mut subscription = clients
        .kinesis(callback, &run.region)
        .subscribe_to_shard()
        .consumer_arn(consumer_arn)
        .shard_id(shard)
        .starting_position(starting_position(cursor)?)
        .send()
        .await
        .map_err(|error| {
            format!(
                "{consumer_arn} could not subscribe to {shard} ({})",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
    let wanted = super::batch_records(run);
    let mut records: Vec<::aws_sdk_kinesis::types::Record> = Vec::new();
    for _ in 0..MAX_SUBSCRIPTION_EVENTS {
        if records.len() >= wanted {
            break;
        }
        let event = subscription.event_stream.recv().await.map_err(|error| {
            format!(
                "the subscription of {consumer_arn} to {shard} failed ({})",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
        // The subscription's stream is over: a consumer renews it from where this read ends,
        // which is what the next pass over this shard does.
        let Some(event) = event else { break };
        if let ::aws_sdk_kinesis::types::SubscribeToShardEventStream::SubscribeToShardEvent(
            delivered,
        ) = event
        {
            records.extend(delivered.records);
        }
    }
    records.truncate(wanted);
    Ok(records)
}

/// The position a subscription of this mapping starts reading `cursor` at, as
/// `SubscribeToShard`'s `StartingPosition` spells one: the same position the pull reader opens
/// its iterator at ([`shard_iterator`]), because where a mapping reads from is the mapping's
/// and not the reader's.
fn starting_position(
    cursor: &ShardCursor,
) -> Result<::aws_sdk_kinesis::types::StartingPosition, String> {
    use ::aws_sdk_kinesis::types::{ShardIteratorType, StartingPosition};

    let position = StartingPosition::builder();
    let position = match cursor {
        ShardCursor::TrimHorizon => position.r#type(ShardIteratorType::TrimHorizon),
        ShardCursor::AtTimestamp(instant) => position
            .r#type(ShardIteratorType::AtTimestamp)
            .timestamp(::aws_sdk_kinesis::primitives::DateTime::from(*instant)),
        ShardCursor::AfterSequenceNumber(sequence_number) => position
            .r#type(ShardIteratorType::AfterSequenceNumber)
            .sequence_number(sequence_number),
    };
    position
        .build()
        .map_err(|error| format!("the position to subscribe from could not be stated ({error})"))
}

/// The iterator that reads `shard` from `cursor`, or `None` for a shard that has no
/// position left to read from.
async fn shard_iterator(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    run: &ShardSourcedMapping,
    shard: &str,
    cursor: &ShardCursor,
) -> Result<Option<String>, String> {
    use ::aws_sdk_kinesis::types::ShardIteratorType;

    let stream_arn = data_stream_arn(run);
    let request = clients
        .kinesis(callback, &run.region)
        .get_shard_iterator()
        .stream_arn(stream_arn)
        .shard_id(shard);
    let request = match cursor {
        ShardCursor::TrimHorizon => request.shard_iterator_type(ShardIteratorType::TrimHorizon),
        ShardCursor::AtTimestamp(instant) => request
            .shard_iterator_type(ShardIteratorType::AtTimestamp)
            .timestamp(::aws_sdk_kinesis::primitives::DateTime::from(*instant)),
        ShardCursor::AfterSequenceNumber(sequence_number) => request
            .shard_iterator_type(ShardIteratorType::AfterSequenceNumber)
            .starting_sequence_number(sequence_number),
    };
    let answer = request.send().await.map_err(|error| {
        format!(
            "no iterator could be taken on {shard} of {stream_arn} ({})",
            ::aws_smithy_types::error::display::DisplayErrorContext(&error)
        )
    })?;
    Ok(answer.shard_iterator().map(str::to_string))
}

/// The records one batch holds: what the shard has at `iterator`, up to the mapping's
/// batch size.
async fn records_at(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    run: &ShardSourcedMapping,
    iterator: &str,
) -> Result<Vec<::aws_sdk_kinesis::types::Record>, String> {
    let answer = clients
        .kinesis(callback, &run.region)
        .get_records()
        .shard_iterator(iterator)
        .limit(run.batch_size)
        .send()
        .await
        .map_err(|error| {
            format!(
                "a batch could not be read from {} ({})",
                data_stream_arn(run),
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
    Ok(answer.records)
}

/// One record as Lambda hands a Kinesis record to a function: the record's own data,
/// partition key, sequence number and arrival instant, under the EVENT SOURCE of the mapping
/// that delivered it.
///
/// `eventSourceARN` carries the mapping's own `EventSourceArn`, unchanged by which reader the
/// batch came through: the data stream for a mapping on a stream, which is the form the
/// published Kinesis record event shows, and the consumer's ARN for a mapping on a stream
/// consumer, whose event source that is.
fn record_envelope(
    record: &::aws_sdk_kinesis::types::Record,
    shard: &str,
    run: &ShardSourcedMapping,
) -> Value {
    let sequence_number = record.sequence_number();
    let mut kinesis = json!({
        "kinesisSchemaVersion": KINESIS_SCHEMA_VERSION,
        "partitionKey": record.partition_key(),
        "sequenceNumber": sequence_number,
        // The record's data travels base64-encoded, as the event a function receives
        // carries it.
        "data": ::aws_smithy_types::base64::encode(record.data().as_ref()),
    });
    if let Some(arrived_at) = record.approximate_arrival_timestamp() {
        kinesis["approximateArrivalTimestamp"] = json!(arrived_at.as_secs_f64());
    }
    let mut envelope = json!({
        "kinesis": kinesis,
        "eventSource": KINESIS_EVENT_SOURCE,
        "eventVersion": KINESIS_EVENT_VERSION,
        // The record is identified by the shard it came from and its own sequence number
        // within that shard.
        "eventID": format!("{shard}:{sequence_number}"),
        "eventName": KINESIS_EVENT_NAME,
        "awsRegion": run.region,
        "eventSourceARN": run.stream_arn,
    });
    if let Some(identity) = run.invoke_identity_arn.as_deref() {
        envelope["invokeIdentityArn"] = json!(identity);
    }
    envelope
}

/// One record as the FILTER sees it: the record's OWN fields, as the Kinesis data plane
/// reports them, with the payload it carries as base64 text read as the document that text
/// spells.
///
/// The criteria decide a record of the SOURCE — the fields that source reported, before the
/// record is wrapped in the envelope an invocation carries ([`super::admitted_of`]) — so a
/// clause names the fields one Kinesis record HAS: `partitionKey`, `sequenceNumber`,
/// `approximateArrivalTimestamp` and `data`. A DynamoDB stream record is decided the same
/// way, and because the record THAT source reports is already the envelope, a clause there
/// names `eventName` and `dynamodb`.
///
/// A record's data is bytes a producer put, which travel base64-encoded, and a clause for
/// that member is written for the document those bytes spell: so the member is read as that
/// document where they spell one — an object or a list, which is what a clause names fields
/// inside — and left as the text it travels as where they do not, a bare scalar and bytes
/// that are no text at all included, so a clause naming fields in such a payload admits the
/// record no more than one whose values disagreed. It is the rule a queue source follows for
/// a structured body ([`crate::state::sqs_poller`]), stated here because the member it is
/// about is this source's own.
pub(super) fn filter_event(envelope: &Value) -> Cow<'_, Value> {
    let mut event = envelope
        .get("kinesis")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let payload = event
        .get("data")
        .and_then(Value::as_str)
        .and_then(|data| ::aws_smithy_types::base64::decode(data).ok())
        .and_then(|bytes| ::serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|document| document.is_object() || document.is_array());
    if let Some(document) = payload {
        event["data"] = document;
    }
    Cow::Owned(event)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{
        data_stream_arn, fan_out_consumer, filter_event, record_envelope, starting_position,
        ShardSourcedMapping,
    };
    use crate::state::event_source_poller::MAX_RECORDS_PER_BATCH;
    use crate::state::{ShardCursor, ShardSource};

    /// A mapping configuration the envelope rows below are built against.
    fn run() -> ShardSourcedMapping {
        run_over("arn:aws:kinesis:us-east-1:000000000000:stream/orders")
    }

    /// A mapping whose event source is `event_source_arn`, with everything else at what a
    /// mapping configuring nothing carries.
    fn run_over(event_source_arn: &str) -> ShardSourcedMapping {
        ShardSourcedMapping {
            source: ShardSource::Kinesis,
            stream_arn: event_source_arn.to_string(),
            region: "us-east-1".to_string(),
            function_arn: "arn:aws:lambda:us-east-1:000000000000:function:consume".to_string(),
            function_name: "consume".to_string(),
            invoke_identity_arn: Some("arn:aws:iam::000000000000:role/consume".to_string()),
            batch_size: MAX_RECORDS_PER_BATCH,
            in_flight_batches: 1,
            filters: Vec::new(),
            reports_batch_item_failures: false,
            attempts_allowed: None,
            bisects_on_function_error: false,
            on_failure_destination: None,
            maximum_record_age: None,
            tumbling_window: None,
            meters_event_counts: false,
            cursors: std::collections::BTreeMap::new(),
            initial_cursor: ShardCursor::TrimHorizon,
        }
    }

    /// RULE: a Kinesis `EventSourceArn` names "the ARN of the data stream or a stream
    /// consumer", so the shards are read on the DATA STREAM either way — the event source
    /// itself, or the stream the consumer's ARN extends — and only an event source naming a
    /// consumer is delivered through that consumer's subscription.
    ///
    /// The rows differ along what the rule generalises over: whether the event source names a
    /// stream or a consumer of one, and how the names and the registration instant of each
    /// are spelled.
    #[test]
    fn an_event_source_naming_a_consumer_reads_the_stream_that_consumer_extends() {
        // (case, the mapping's event source, the stream its shards are read on, the consumer
        // its records are delivered through)
        let rows: Vec<(&str, &str, &str, Option<&str>)> = vec![
            (
                "a-data-stream",
                "arn:aws:kinesis:us-east-1:000000000000:stream/orders",
                "arn:aws:kinesis:us-east-1:000000000000:stream/orders",
                None,
            ),
            (
                "a-stream-consumer",
                "arn:aws:kinesis:us-east-1:000000000000:stream/orders/consumer/fanout:1700000000",
                "arn:aws:kinesis:us-east-1:000000000000:stream/orders",
                Some(
                    "arn:aws:kinesis:us-east-1:000000000000:stream/orders/consumer/fanout:\
                     1700000000",
                ),
            ),
            (
                "a-consumer-whose-name-carries-the-separators-the-pattern-admits",
                "arn:aws:kinesis:eu-west-2:000000000000:stream/order.events-2/consumer/\
                 fan_out.reader-1:1",
                "arn:aws:kinesis:eu-west-2:000000000000:stream/order.events-2",
                Some(
                    "arn:aws:kinesis:eu-west-2:000000000000:stream/order.events-2/consumer/\
                     fan_out.reader-1:1",
                ),
            ),
        ];
        for (case, event_source, stream, consumer) in rows {
            let run = run_over(event_source);
            assert_eq!(
                data_stream_arn(&run),
                stream,
                "case {case:?}: expected the shards to be read on {stream}, got {}",
                data_stream_arn(&run)
            );
            assert_eq!(
                fan_out_consumer(&run),
                consumer,
                "case {case:?}: expected the delivery to go through {consumer:?}, got {:?}",
                fan_out_consumer(&run)
            );
        }
    }

    /// RULE: where a mapping reads from is the MAPPING's, so the position a subscription
    /// starts at is the one the pull reader would open its iterator at — the same
    /// `ShardIteratorType`, carrying the same member.
    ///
    /// The rows differ along what the rule generalises over: which of the positions a
    /// mapping's cursor can stand at it is.
    #[test]
    fn a_subscription_starts_at_the_position_the_mapping_carries() {
        let instant = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // (case, the cursor, the position type it subscribes from, the sequence number and the
        // timestamp that position carries)
        let rows: Vec<(&str, ShardCursor, &str, Option<&str>, bool)> = vec![
            (
                "the-oldest-retained-record",
                ShardCursor::TrimHorizon,
                "TRIM_HORIZON",
                None,
                false,
            ),
            (
                "a-stated-instant",
                ShardCursor::AtTimestamp(instant),
                "AT_TIMESTAMP",
                None,
                true,
            ),
            (
                "past-a-record-already-processed",
                ShardCursor::AfterSequenceNumber("4959033827149025660855969253836157".to_string()),
                "AFTER_SEQUENCE_NUMBER",
                Some("4959033827149025660855969253836157"),
                false,
            ),
        ];
        for (case, cursor, position_type, sequence_number, stamped) in rows {
            let position = starting_position(&cursor)
                .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
            assert_eq!(
                position.r#type.as_str(),
                position_type,
                "case {case:?}: expected to subscribe from {position_type}, got {:?}",
                position.r#type
            );
            assert_eq!(
                position.sequence_number.as_deref(),
                sequence_number,
                "case {case:?}: expected the sequence number {sequence_number:?}, got {:?}",
                position.sequence_number
            );
            assert_eq!(
                position.timestamp.is_some(),
                stamped,
                "case {case:?}: expected a stated instant: {stamped}, got {:?}",
                position.timestamp
            );
        }
    }

    /// One record as the Kinesis data plane answers it.
    fn record(
        sequence_number: &str,
        data: &[u8],
        arrived_at: Option<SystemTime>,
    ) -> ::aws_sdk_kinesis::types::Record {
        let mut builder = ::aws_sdk_kinesis::types::Record::builder()
            .sequence_number(sequence_number)
            .partition_key("orders")
            .data(::aws_sdk_kinesis::primitives::Blob::new(data.to_vec()));
        if let Some(arrived_at) = arrived_at {
            builder = builder.approximate_arrival_timestamp(
                ::aws_sdk_kinesis::primitives::DateTime::from(arrived_at),
            );
        }
        builder.build().expect("the record is well formed")
    }

    /// RULE: each record of a batch is handed over as its OWN envelope, carrying that
    /// record's own data base64-encoded, its partition key, its sequence number and the
    /// stream it came from.
    ///
    /// The rows differ along what the rule generalises over: what the record's data is.
    #[test]
    fn a_record_is_handed_over_carrying_its_own_data() {
        let run = run();
        let arrived_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // (case, the record's data)
        let rows: Vec<(&str, &[u8])> = vec![
            ("one-word", b"first"),
            ("a-longer-payload", b"{\"order\":\"abc\"}"),
            ("an-empty-payload", b""),
        ];
        for (case, data) in rows {
            let sequence_number = "49590338271490256608559692538361571095921575989136588898";
            let record = record(sequence_number, data, Some(arrived_at));
            let envelope = record_envelope(&record, "shardId-000000000000", &run);
            let encoded = ::aws_smithy_types::base64::encode(data);
            assert_eq!(
                envelope["kinesis"]["data"],
                ::serde_json::json!(encoded),
                "case {case:?}: the record must carry its own data base64-encoded, got \
                 {envelope:?}"
            );
            assert_eq!(
                envelope["eventSourceARN"],
                ::serde_json::json!(run.stream_arn),
                "case {case:?}: the record must name the stream it came from, got {envelope:?}"
            );
            assert_eq!(
                envelope["kinesis"]["sequenceNumber"],
                ::serde_json::json!(sequence_number),
                "case {case:?}: the record must carry its own sequence number, got {envelope:?}"
            );
            assert_eq!(
                envelope["eventID"],
                ::serde_json::json!(format!("shardId-000000000000:{sequence_number}")),
                "case {case:?}: the record must be identified by its shard and sequence \
                 number, got {envelope:?}"
            );
        }
    }

    /// RULE: the record a filter is matched against carries the record's OWN fields, as the
    /// Kinesis data plane reports them, with the payload it carries as base64 text read as the
    /// document that text spells; every other payload stays the text the record travels it as.
    ///
    /// The rows differ along what the rule generalises over: what the payload holds — an
    /// object, an array, a bare scalar, text that is no document at all, bytes that are no
    /// text at all, and no payload at all.
    #[test]
    fn a_filter_sees_the_records_own_fields_with_its_payload_as_the_document_it_spells() {
        let run = run();
        let sequence_number = "49590338271490256608559692538361571095921575989136588898";
        // (case, the record's data, what the filter must see under `data`; `None` where the
        // bytes spell no document and the member stays the text it travels as)
        let rows: Vec<(&str, &[u8], Option<::serde_json::Value>)> = vec![
            (
                "a-json-object",
                br#"{"city":"Seattle","count":2}"#,
                Some(::serde_json::json!({ "city": "Seattle", "count": 2 })),
            ),
            (
                "a-json-array",
                b"[1,2,3]",
                Some(::serde_json::json!([1, 2, 3])),
            ),
            // A bare scalar names nothing to descend into, so it stays the text the member
            // travels as.
            ("a-bare-scalar", b"12", None),
            ("text-that-is-no-document", b"not a document", None),
            ("bytes-that-are-no-text-at-all", b"\x00\xff\x80payload", None),
            ("an-empty-payload", b"", None),
        ];
        for (case, data, expected) in rows {
            let record = record(sequence_number, data, None);
            let envelope = record_envelope(&record, "shardId-000000000000", &run);
            let event = filter_event(&envelope);
            let expected = expected
                .unwrap_or_else(|| ::serde_json::json!(::aws_smithy_types::base64::encode(data)));
            assert_eq!(
                event["data"], expected,
                "case {case:?}: expected the filter to see data {expected}, got {event}"
            );
            // The record's other fields are the ones the source reported for it.
            assert_eq!(
                event["partitionKey"],
                ::serde_json::json!("orders"),
                "case {case:?}: expected the record's own partition key, got {event}"
            );
            assert_eq!(
                event["sequenceNumber"],
                ::serde_json::json!(sequence_number),
                "case {case:?}: expected the record's own sequence number, got {event}"
            );
        }
    }
}
