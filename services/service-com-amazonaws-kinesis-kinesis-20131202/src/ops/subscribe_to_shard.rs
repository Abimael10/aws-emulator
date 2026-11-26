// SPDX-License-Identifier: CC0-1.0

//! @op SubscribeToShard
//!
//! `POST / -> 200` (`X-Amz-Target: Kinesis_20131202.SubscribeToShard`). Subscribes one
//! registered enhanced fan-out consumer to one shard of the stream its `ConsumerARN` extends
//! and answers the modelled `SubscribeToShardEventStream` as a `vnd.amazon.eventstream` body
//! of `SubscribeToShardEvent` frames. This is the ONLY file to touch for this operation.
//!
//! This operation is hand-authored rather than generated: its output member `EventStream`
//! targets a `@streaming` union, which the op-stub generator declines
//! (`src/ops/SKIPPED_STUBS.md`) and whose generated `ErasedOperation` shim declines the
//! request. The framing and the routing seam are `service_common::event_stream` and
//! `src/service.rs`; this file holds the behaviour, and the shapes are in
//! `src/ops/types/subscribe_to_shard_types.rs`.
//!
//! ## What a subscription answers
//!
//! "This operation establishes an HTTP/2 connection between the consumer you specify in the
//! ConsumerARN parameter and the shard you specify in the ShardId parameter. After the
//! connection is successfully established, Kinesis Data Streams pushes records from the shard
//! to the consumer over this connection." A subscription this emulator serves is one RESPONSE
//! whose body is that stream: the frame a client reads the output shape out of
//! (`service_common::event_stream::INITIAL_RESPONSE_EVENT`, carrying no member, because the
//! stream is this output's only one) and then an event every [`PUSH_INTERVAL`] for as long as
//! the subscription HOLDS its shard — not one event and done, because a consumer reading an
//! idle shard is pushed one about every second and a consumer reading the tip of one is
//! pushed what arrives while it reads.
//!
//! Every one of those events is its own read of the stored shard, never synthesised, by the
//! same shared read of a shard `GetRecords` serves its page from
//! ([`crate::state::read_shard_from`]) from the position the one before it left off at — so
//! the push reader and the pull reader deliver the same records in the same order, and a
//! record WRITTEN while the subscription is being served is delivered by the next event
//! rather than missed.
//!
//! The records one event carries are bounded by the ceilings that shared read is given
//! ([`max_event_records`] / [`MAX_EVENT_BYTES`]), and the event reports where the reader now
//! stands: `ContinuationSequenceNumber` — "Use this as SequenceNumber in the next call to
//! SubscribeToShard, with StartingPosition set to AT_SEQUENCE_NUMBER or
//! AFTER_SEQUENCE_NUMBER. Use ContinuationSequenceNumber for checkpointing because it
//! captures your shard progress even when no data is written to the shard" — and
//! `MillisBehindLatest`, how far behind the tip of the shard the subscription is.
//! `ChildShards` is carried only "at the end of the current shard", which is a shard a
//! reshard closed and whose retained records the subscription has delivered; that event
//! carries no `ContinuationSequenceNumber` at all ([`ShardEndEvent`]) — a shard that is over
//! has nowhere to continue from — and it is the last event of the subscription.
//!
//! ## What one subscription holds, and for how long
//!
//! A subscription is RECORDED on the consumer it was established for while its event stream
//! is LIVE ([`crate::state::ConsumerRecord::subscribed_at`]), because a subscription being
//! contended is what the next call is judged against: "If you call SubscribeToShard again
//! with the same ConsumerARN and ShardId within 5 seconds of a successful call, you'll get a
//! ResourceInUseException. If you call SubscribeToShard 5 seconds or more after a successful
//! call, the second call takes over the subscription." That window
//! ([`crate::state::SUBSCRIPTION_TAKEOVER_WINDOW`]) is as long as this service calls a
//! subscription contended, so it is how long one goes on pushing: a caller reading one of
//! these streams gets the events of one such span and renews from the
//! `ContinuationSequenceNumber` the last of them reported, which is the loop the service
//! documents for the 5 minutes a real connection lives — "your consumer starts receiving
//! events of type SubscribeToShardEvent over the HTTP/2 connection for up to 5 minutes, after
//! which time you need to call SubscribeToShard again to renew the subscription if you want
//! to continue to receive records".
//!
//! The record is RELEASED the moment that event stream ends
//! ([`crate::state::ConsumerRecord::subscription_ended`]), which is the instant its frames are
//! complete. A consumer whose stream is over holds no shard, so the renewal above is SERVED.
//!
//! ## Two subscriptions of one consumer to one shard
//!
//! A subscription established while another one of the same consumer to the same shard is
//! still being read CONTENDS with it, and that is not a refusal of the call: the call is
//! served with a live event stream of its own, and the conflict travels as the
//! `ResourceInUseException` member of the output union — "If you call SubscribeToShard again
//! with the same ConsumerARN and ShardId within 5 seconds of a successful call, you'll get a
//! ResourceInUseException. If you call SubscribeToShard 5 seconds or more after a successful
//! call, the second call takes over the subscription and the previous connection expires or
//! fails with a ResourceInUseException."
//!
//! BOTH streams report it and end there: the contesting one as the only event it carries
//! (it reads no record, [`Established::Contested`]), and the one it contested as the last
//! event of its own stream, which its pushing loop frames as soon as it sees the mark
//! ([`crate::state::ConsumerRecord::subscription_contested`]). That is the one signal an
//! enhanced fan-out consumer tells "my lease is contested, stop and re-acquire" from "my
//! window elapsed, renew from my checkpoint" by, so two readers racing onto one consumer and
//! shard are both told rather than both quietly served the same records. The shard is released
//! when the contested stream ends, so a later `SubscribeToShard` for the same consumer and
//! shard is served again from the position it names.
//!
//! ## Input
//!
//! * `ConsumerARN` (ConsumerARN, len 1..2048, pattern
//!   `^(arn):aws.*:kinesis:.*:\d{12}:.*stream\/[a-zA-Z0-9_.-]+\/consumer\/[a-zA-Z0-9_.-]+:[0-9]+$`)
//!   **required** — the consumer to subscribe. "Before you call this operation, call
//!   RegisterStreamConsumer to register the consumer with Kinesis Data Streams." A consumer's
//!   ARN is its stream's ARN extended, so it names the stream too
//!   ([`crate::state::stream_arn_of_consumer`]).
//! * `ShardId` (ShardId, len 1..128, pattern `^[a-zA-Z0-9_.-]+$`) **required** — the shard of
//!   that stream to read.
//! * `StartingPosition` (StartingPosition) **required** — "The starting position in the data
//!   stream from which to start streaming", one of the five `ShardIteratorType` positions
//!   with the member each of them reads ([`crate::state::requested_shard_position`]).
//! * `StreamId` (StreamId, len 1..24, pattern `^[a-z0-9]{20}-[a-z0-9]{3}$`) — "Not
//!   Implemented. Reserved for future use", so no stream carries one.
//! * `DryRun` (BooleanObject) — "Checks if your request will succeed."
//!
//! ## Output
//!
//! * `EventStream` (SubscribeToShardEventStream, `@streaming`) **required** — the body. Its
//!   `SubscribeToShardEvent` member is the data event; its `ResourceInUseException` member is
//!   the conflict between two subscriptions of one consumer to one shard; its
//!   `KMSDisabledException` member and the rest of the KMS family are in-band failures of
//!   reading a stream encrypted under a CMK the data plane may no longer use, which is why no
//!   KMS error is among this operation's own modelled errors.
//!
//! ## Errors
//!
//! * `ResourceNotFoundException` (HTTP 400) — the `ConsumerARN` names a stream this account
//!   and region do not hold, a consumer that stream does not carry, or the `ShardId` names no
//!   shard of it.
//! * `ResourceInUseException` (HTTP 400) — the stream is not `ACTIVE`, or the consumer is not
//!   `ACTIVE` yet ("a consumer can't read data while in the CREATING or DELETING states"). A
//!   subscription contended by another one of the same consumer to the same shard is NOT
//!   refused: it is served, and the same shape travels as an in-band event of the stream.
//! * `InvalidArgumentException` (HTTP 400) — the `StartingPosition` members name no position
//!   in the shard.
//! * `DryRunOperationException` (HTTP 400) — "The request was rejected because the DryRun
//!   parameter was specified."
//! * `AccessDeniedException`, `LimitExceededException` (HTTP 400) — this operation's
//!   remaining modelled errors; no condition of this store raises them.
//! * `InternalFailureException` (HTTP 500) — widened in from the service's own error set: the
//!   only honest answer for a service mounted on an engine that holds no state for it.

#[allow(unused_imports)]
use super::types::*;
use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema_server::ProtocolError;
use service_common::event_stream::{
    error_response, event_stream_only_response, frame_empty_initial_response, frame_event,
    read_request, RequestRefusal,
};
use smithy_server_runtime::{BoxFuture, OperationContext, Refusal, Service};
use std::sync::Arc;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative `smithy!`
// macro, which this line is what emits into the module.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-kinesis-kinesis-20131202.json");

/// The operation's MODELED errors; `#[operation_error]` accepts only variants the model
/// declares for it, and every one of them is a client error at HTTP 400.
#[smithy_macros::operation_error(
    "com.amazonaws.kinesis#SubscribeToShard",
    // `InternalFailureException` is widened in with `extra_errors`: this SERVICE models it
    // ("The processing of the request failed because of an unknown error, exception, or
    // failure"), the operation shape does not list it, and it is the only honest answer for a
    // service mounted on an engine that holds no state for it.
    extra_errors = ["InternalFailureException"]
)]
#[derive(Debug)]
pub enum SubscribeToShardErrorModeled {
    AccessDeniedException(AccessDeniedException),
    DryRunOperationException(DryRunOperationException),
    InvalidArgumentException(InvalidArgumentException),
    LimitExceededException(LimitExceededException),
    ResourceInUseException(ResourceInUseException),
    ResourceNotFoundException(ResourceNotFoundException),
    InternalFailureException(InternalFailureException),
}

/// The `SubscribeToShardEventStream` member a data event is framed as, which the frame
/// carries in its `:event-type` header (source: `smithy_query(query="shape",
/// shape="SubscribeToShardEventStream")`).
const SUBSCRIBE_TO_SHARD_EVENT: &str = "SubscribeToShardEvent";

/// The `SubscribeToShardEventStream` member an in-band failure to read the stream's records
/// under its CMK is framed as (same source).
const KMS_DISABLED_EVENT: &str = "KMSDisabledException";

/// The `SubscribeToShardEventStream` member the conflict between two subscriptions of one
/// consumer to one shard is framed as (same source): "The resource is not available for this
/// operation" is a member of the output union as well as one of this operation's errors, so a
/// subscription that is CONTESTED reports it as the last event of its own stream instead of
/// the call being refused.
const RESOURCE_IN_USE_EVENT: &str = "ResourceInUseException";

/// The members a `SubscribeToShard` request spells a starting position with, as its refusals
/// name them: this operation carries all three inside its `StartingPosition` structure.
const POSITION_MEMBERS: crate::state::PositionMembers = crate::state::PositionMembers {
    position_type: "StartingPosition.Type",
    sequence_number: "StartingPosition.SequenceNumber",
    timestamp: "StartingPosition.Timestamp",
};

/// The most bytes of record DATA one `SubscribeToShardEvent` carries: the ceiling a single
/// pull read of a shard answers with ("The maximum size of data that GetRecords can return is
/// 10 MiB", `GetRecords`), because what bounds one delivery of a shard's records is a
/// property of the shard rather than of which reader asked for them. An event that reached it
/// still carries its first record, and its `ContinuationSequenceNumber` is what the consumer
/// reads on from.
const MAX_EVENT_BYTES: usize = crate::state::MAX_GET_RECORDS_BYTES;

/// The most records one such event carries, for the same reason ("The maximum number of
/// records that can be returned per call is 10,000", `GetRecords`).
fn max_event_records() -> usize {
    usize::try_from(crate::state::MAX_GET_RECORDS_LIMIT).unwrap_or(1)
}

/// The `SubscribeToShard` operation.
///
/// Owns the shared engine (the constructor seam). The generated routing table holds one of
/// these and `service.rs` holds another wrapped in an `EventStreamRoute`; both reach the same
/// state, because the state lives in the shared engine, not on the operation.
pub struct SubscribeToShard<Eng> {
    engine: Arc<Eng>,
}

impl<Eng> SubscribeToShard<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any request
    /// runs.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

/// Refuse the request with the error this operation models for a parameter that "exceeds its
/// restrictions, is not supported, or can't be used".
fn invalid_argument(message: String) -> SubscribeToShardErrorModeled {
    SubscribeToShardErrorModeled::InvalidArgumentException(InvalidArgumentException {
        message: Some(message),
    })
}

/// The modelled refusal for a resource this account and region do not hold: "The requested
/// resource could not be found. The stream might not be specified correctly."
fn not_found(message: String) -> SubscribeToShardErrorModeled {
    SubscribeToShardErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
        message: Some(message),
    })
}

/// The modelled refusal for a resource that is not available for this operation: "For
/// successful operation, the resource must be in the ACTIVE state" (`ResourceInUseException`).
fn in_use(message: String) -> SubscribeToShardErrorModeled {
    SubscribeToShardErrorModeled::ResourceInUseException(ResourceInUseException {
        message: Some(message),
    })
}

/// A failure of this emulator rather than of the request.
fn internal(message: String) -> SubscribeToShardErrorModeled {
    SubscribeToShardErrorModeled::InternalFailureException(InternalFailureException {
        message: Some(message),
    })
}

/// ONE event of a live subscription, with what the subscription needs to go on pushing: where
/// the next event reads from and what this delivery measured.
struct Pushed {
    /// The event itself, framed into the body as it is produced.
    event: PushedEvent,
    /// Where the event AFTER this one starts reading ([`SubscribeToShard::pushed`]).
    next: crate::state::ShardPosition,
    /// The `AWS/Kinesis` observations this delivery made, published off the answer.
    measured: crate::state::Measured,
}

/// What establishing a subscription answered with: the shard it HOLDS and the first event of
/// reading it, or the conflict it found.
enum Established {
    /// The subscription holds its shard: this is the first event it pushed, and it goes on
    /// pushing while it holds it.
    Holding(Pushed),
    /// Another subscription of this consumer to this shard is still being read, so the two
    /// contend: this one is SERVED — "the second call takes over the subscription and the
    /// previous connection expires or fails with a ResourceInUseException" — and both event
    /// streams report the conflict in band and end. It reads no record and holds no shard, so
    /// neither of two contending readers quietly delivers the shard twice.
    Contested(ResourceInUseException),
}

/// Which of the two forms one pushed event takes.
enum PushedEvent {
    /// An event of a shard the subscription goes on reading: it reports the checkpoint the
    /// consumer reads on from.
    Open(SubscribeToShardEvent),
    /// The event that ends a CLOSED shard, carrying the children the consumer reads on in and
    /// no checkpoint. The shard's end is the subscription's end, so nothing is pushed after
    /// it.
    ShardEnd(ShardEndEvent),
}

impl PushedEvent {
    /// This event as ONE `vnd.amazon.eventstream` frame, through the service's own protocol:
    /// both forms are the one modelled `SubscribeToShardEvent` member of the output union, so
    /// they are framed under the same `:event-type` and differ only in the members they carry.
    fn framed(
        &self,
        protocol: &dyn ::smithy_server_runtime::ServerHttpProtocol,
    ) -> Result<Vec<u8>, ProtocolError> {
        match self {
            Self::Open(event) => frame_event(
                protocol,
                SUBSCRIBE_TO_SHARD_EVENT,
                false,
                event,
                &SubscribeToShardEvent::SCHEMA,
            ),
            Self::ShardEnd(event) => frame_event(
                protocol,
                SUBSCRIBE_TO_SHARD_EVENT,
                false,
                event,
                &ShardEndEvent::SCHEMA,
            ),
        }
    }

    /// Whether this event is the end of the shard, and so the end of the subscription.
    fn ends(&self) -> bool {
        matches!(self, Self::ShardEnd(_))
    }
}

/// How often a live subscription pushes an event: a consumer reading an IDLE shard is sent one
/// about every second, which is what makes a subscription over a shard nothing is writing to
/// observable at all (recorded behaviour; the model says only that "Kinesis Data Streams
/// pushes records from the shard to the consumer over this connection").
const PUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// The most events ONE subscription pushes: its span
/// ([`crate::state::SUBSCRIPTION_TAKEOVER_WINDOW`], as long as this service calls a
/// subscription contended) over [`PUSH_INTERVAL`], plus the one pushed the moment it is
/// established.
///
/// A count rather than only a deadline, so what one response can hold is a constant of this
/// crate whatever the clock does: a wall clock that steps backwards cannot make a
/// subscription push more than this.
const MAX_PUSHED_EVENTS: u32 = 1 + (crate::state::SUBSCRIPTION_TAKEOVER_WINDOW.as_millis()
    / PUSH_INTERVAL.as_millis()) as u32;

impl<Eng: Send + Sync + 'static> SubscribeToShard<Eng> {
    /// Resolve the subscription the request asks for, refuse whatever stands in the way, and
    /// RECORD the subscription it established — or, where another subscription of this
    /// consumer to this shard is still being read, the CONFLICT the two of them are reported
    /// with ([`Established`]).
    ///
    /// The whole resolution lives here so the store's lock is held for exactly this long,
    /// which is also what decides which of two overlapping subscriptions of the same consumer
    /// to the same shard holds the shard: the one that gets the lock first records it, and the
    /// next one reads that record and contests it.
    fn establish(
        store: &mut crate::state::ComAmazonawsKinesisKinesis20131202State,
        region: &str,
        now: std::time::SystemTime,
        input: &SubscribeToShardInput,
    ) -> Result<Established, SubscribeToShardErrorModeled> {
        // "Before you call this operation, call RegisterStreamConsumer to register the
        // consumer with Kinesis Data Streams": the consumer's ARN names the stream it was
        // registered with, so the stream is resolved first and the consumer inside it — a
        // consumer of a stream this account and region do not hold is not found under the
        // stream, which is what the refusal states.
        let stream_arn = crate::state::stream_arn_of_consumer(&input.consumer_arn);
        let stream = store
            .stream_by_arn(now, region, stream_arn)
            .ok_or_else(|| not_found(crate::state::stream_not_found_message(stream_arn)))?;
        let stream_key = crate::state::RegionalName::new(&stream.region, &stream.name);
        // The stream's own name, which names it in the conflict two contending subscriptions
        // of one of its consumers are reported with below.
        let stream_name = stream.name.clone();
        // Which `AWS/Kinesis` series this delivery is measured into: the stream's name, and
        // the shard-level metrics its enhanced monitoring names
        // (`crate::state::stream_metrics`).
        let series = crate::state::StreamSeries::of(stream);

        // A stream whose create, removal or re-tiling is still due serves no shard to read,
        // which is the modelled "the resource must be in the ACTIVE state" refusal. A stream
        // whose shard count is being changed DOES serve the read: "you can continue to read
        // and write data to your stream while its status is UPDATING" (`UpdateShardCount`).
        let status = stream.status(now);
        if !stream.serves_shards(now) {
            return Err(in_use(crate::state::stream_not_active_message(
                &stream.name,
                status,
            )));
        }

        let consumer = stream
            .consumer_by_arn(&input.consumer_arn)
            .ok_or_else(|| {
                not_found(crate::state::consumer_not_found_message(
                    &input.consumer_arn,
                    &stream.arn,
                ))
            })?;

        // "A consumer can't read data while in the CREATING or DELETING states"
        // (`Consumer$ConsumerStatus`), so a consumer whose registration has not settled yet
        // is not available for this operation.
        let consumer_status = consumer.status(now);
        if consumer_status != crate::state::CONSUMER_ACTIVE {
            return Err(in_use(crate::state::consumer_not_active_message(
                &consumer.name,
                consumer_status,
            )));
        }

        // "If you call SubscribeToShard again with the same ConsumerARN and ShardId within 5
        // seconds of a successful call, you'll get a ResourceInUseException": a subscription
        // established while another one of this consumer to this shard is still being read
        // CONTENDS with it, and that is not a refusal of the call — it is served, and the
        // conflict is reported on both event streams by the union member below. Only a stream
        // that has not ended yet is recorded at all, so a consumer renewing from its
        // checkpoint after its stream is over contends with nothing.
        //
        // Read here, where the whole request is judged, and acted on once the request is
        // known to be servable at all: a subscription naming a shard this stream does not
        // hold, a starting position the shard does not admit, or a dry run is answered with
        // the error the model declares for it whether or not another subscription is live.
        let contends = consumer.holds_subscription(&input.shard_id, now);
        let consumer_name = consumer.name.clone();

        let shard = stream
            .shards
            .iter()
            .find(|shard| shard.shard_id == input.shard_id)
            .ok_or_else(|| {
                // A shard of another stream, and a shard id nothing laid down, are the same
                // thing to this stream: a shard it does not hold.
                not_found(crate::state::shard_not_found_message(
                    &input.shard_id,
                    &stream.name,
                ))
            })?;

        // Where in the shard the subscription starts, by this service's one rule for naming a
        // position in a shard — the rule `GetShardIterator` resolves its iterator with.
        let trim_horizon = stream.trim_horizon(now);
        let position = crate::state::requested_shard_position(
            POSITION_MEMBERS,
            input.starting_position.r#type.as_str(),
            input.starting_position.sequence_number.as_deref(),
            input.starting_position.timestamp,
            shard,
            &stream.shards,
            trim_horizon,
        )
        .map_err(invalid_argument)?;

        // "Checks if your request will succeed. DryRun is an optional parameter"
        // (`SubscribeToShardInput$DryRun`): the request is judged in full above and then
        // refused with the error the model declares for it, stating that it WOULD have
        // succeeded, so a dry run establishes no subscription and reads no record.
        if input.dry_run.unwrap_or(false) {
            return Err(SubscribeToShardErrorModeled::DryRunOperationException(
                DryRunOperationException {
                    message: Some(crate::state::dry_run_succeeded_message(
                        "SubscribeToShard",
                    )),
                },
            ));
        }

        // A subscription that contends with one this consumer is still reading reads no
        // record of its own and holds no shard: both it and the one it contests are about to
        // report the conflict in band and end, so the only thing it writes is the MARK that
        // tells the incumbent's pushing loop it has been contested. The shard stays held by
        // the subscription that was already reading it until that stream is over, which is
        // what frees it for the next call.
        if contends {
            let fault = crate::state::another_active_subscription_message(
                &stream_name,
                &input.shard_id,
                &consumer_name,
            );
            if let Some(consumer) = store
                .streams
                .get_mut(&stream_key)
                .and_then(|stream| stream.consumer_by_arn_mut(&input.consumer_arn))
            {
                consumer.contend_subscription(&input.shard_id, now);
            }
            return Ok(Established::Contested(ResourceInUseException {
                message: Some(fault),
            }));
        }

        // The first event of the subscription, and where it reads on from.
        let pushed = Self::pushed(stream, shard, &position, now);

        // What the subscription ITSELF measured, beside what each of its events does
        // ([`Self::pushed`]): one established subscription.
        let mut measured = pushed.measured;
        measured.record_for_the_call(
            &series,
            crate::state::SUBSCRIBE_TO_SHARD_SUCCESS,
            1.0,
            now,
        );

        // The subscription is established and is now being served, so it is recorded on the
        // consumer it was established for: that is what another call to the same consumer
        // and shard, while this one is still live, is judged against.
        if let Some(consumer) = store
            .streams
            .get_mut(&stream_key)
            .and_then(|stream| stream.consumer_by_arn_mut(&input.consumer_arn))
        {
            consumer.subscribed_at(&input.shard_id, now);
        }

        Ok(Established::Holding(Pushed { measured, ..pushed }))
    }

    /// ONE event of a live subscription over `shard`: the records the shard retains from
    /// `position`, where the subscription reads on from, whether this is the shard's end, and
    /// what the delivery measured.
    ///
    /// The records are read out of the stored shard by the same shared read a `GetRecords`
    /// page is served from ([`crate::state::read_shard_from`]), under the ceilings that read
    /// is given ([`max_event_records`] / [`MAX_EVENT_BYTES`]), so the push reader and the
    /// pull reader deliver the same records in the same order.
    fn pushed(
        stream: &crate::state::Stream,
        shard: &crate::state::ShardRecord,
        position: &crate::state::ShardPosition,
        now: std::time::SystemTime,
    ) -> Pushed {
        // Which `AWS/Kinesis` series this delivery is measured into: the stream's name, and
        // the shard-level metrics its enhanced monitoring names
        // (`crate::state::stream_metrics`).
        let series = crate::state::StreamSeries::of(stream);
        // A record outside the stream's retention window is trimmed, so it is not among the
        // records any position of this shard reads.
        let trim_horizon = stream.trim_horizon(now);
        let retained: Vec<&crate::state::DataRecord> = shard.retained(trim_horizon).collect();
        let read = crate::state::read_shard_from(
            position,
            &retained,
            max_event_records(),
            MAX_EVENT_BYTES,
            now,
        );
        // "The list of the child shards of the current shard, returned only at the end of
        // the current shard": a shard is closed by a reshard, and a consumer that has been
        // delivered everything a closed one retains reads on from the shards that reshard
        // produced from it.
        let child_shards = (!shard.is_open() && read.drained)
            .then(|| stream.child_shards_of(&shard.shard_id))
            .filter(|children| !children.is_empty());
        // That end is where the SUBSCRIPTION ends too: there is nothing left on this shard
        // to push, and what the consumer does next is subscribe to the children the event
        // names.
        let records: Vec<Record> = read.records.iter().map(|record| record.to_shape()).collect();
        let event = match child_shards {
            Some(child_shards) => PushedEvent::ShardEnd(ShardEndEvent {
                records,
                millis_behind_latest: read.millis_behind_latest,
                child_shards: Some(child_shards),
            }),
            None => PushedEvent::Open(SubscribeToShardEvent {
                records,
                // "Use ContinuationSequenceNumber for checkpointing because it captures your
                // shard progress even when no data is written to the shard": the newest
                // record this consumer has now seen, and the shard's own first sequence
                // number while it has seen none — every record the shard can hold is numbered
                // past that one, so reading on AFTER it delivers all of them.
                continuation_sequence_number: read.last_seen.map_or_else(
                    || shard.starting_sequence_number.clone(),
                    |record| record.sequence_number.clone(),
                ),
                millis_behind_latest: read.millis_behind_latest,
                // A shard that is still being read reports no children: "The list of the
                // child shards of the current shard, returned only at the end of the current
                // shard."
                child_shards: None,
            }),
        };

        // Where the NEXT event of this subscription starts: just after the newest record
        // this one delivered, and the position this one carried while it delivered none —
        // which is how a record written DURING the subscription is picked up by the event
        // after it.
        let next = read.last_seen.map_or_else(
            || position.clone(),
            |record| {
                crate::state::ShardPosition::AfterSequenceNumber(record.sequence_number.clone())
            },
        );

        // What this delivery did, as the `AWS/Kinesis` series report it: the records it
        // retrieved from the shard (stream-level, and shard-level where the stream's enhanced
        // monitoring names the metric — a record retrieved from a shard is counted the same
        // way whichever reader asked for it) and the `SubscribeToShardEvent` families of the
        // event itself. Collected here, under the store guard, and published by the caller
        // once that guard is released.
        let delivered_records = crate::state::counted(read.records.len());
        let delivered_bytes =
            crate::state::counted(read.records.iter().fold(0usize, |total, record| {
                total.saturating_add(record.stored_bytes())
            }));
        let lag = f64::from(i32::try_from(read.millis_behind_latest).unwrap_or(i32::MAX));
        let mut measured = crate::state::Measured::new();
        measured.record_outgoing(
            &series,
            &shard.shard_id,
            delivered_records,
            delivered_bytes,
            now,
        );
        measured.record(
            &series,
            crate::state::ITERATOR_AGE,
            &shard.shard_id,
            lag,
            now,
        );
        for (event_series, value) in [
            (
                crate::state::SUBSCRIBE_TO_SHARD_EVENT_RECORDS,
                delivered_records,
            ),
            (
                crate::state::SUBSCRIBE_TO_SHARD_EVENT_BYTES,
                delivered_bytes,
            ),
            (crate::state::SUBSCRIBE_TO_SHARD_EVENT_LAG, lag),
        ] {
            measured.record_for_the_call(&series, event_series, value, now);
        }

        Pushed {
            event,
            next,
            measured,
        }
    }

    /// The event the subscription pushes NEXT, read from the store again at `now`, or `None`
    /// when there is nothing left to push it over: a stream, consumer or shard that has gone
    /// away while the subscription was being served ends the stream rather than failing it.
    fn pushed_again(
        store: &crate::state::ComAmazonawsKinesisKinesis20131202State,
        region: &str,
        now: std::time::SystemTime,
        input: &SubscribeToShardInput,
        position: &crate::state::ShardPosition,
    ) -> Option<Pushed> {
        let stream_arn = crate::state::stream_arn_of_consumer(&input.consumer_arn);
        let stream = store.stream_by_arn(now, region, stream_arn)?;
        if !stream.serves_shards(now) {
            return None;
        }
        stream.consumer_by_arn(&input.consumer_arn)?;
        let shard = stream
            .shards
            .iter()
            .find(|shard| shard.shard_id == input.shard_id)?;
        Some(Self::pushed(stream, shard, position, now))
    }

    /// The conflict the subscription this request established at `established_at` must report
    /// IN BAND, when another subscription of the same consumer to the same shard was
    /// established behind it while it was being served, and `None` while nothing contests it.
    ///
    /// Worded from the stored records the conflict is between, so the account, the stream, the
    /// shard and the consumer that contend are what name it. A consumer, stream or shard that
    /// has gone away in the meantime contests nothing — that stream simply ends
    /// ([`Self::pushed_again`]).
    fn contested(
        store: &crate::state::ComAmazonawsKinesisKinesis20131202State,
        region: &str,
        now: std::time::SystemTime,
        input: &SubscribeToShardInput,
        established_at: std::time::SystemTime,
    ) -> Option<ResourceInUseException> {
        let stream = store.stream_by_arn(
            now,
            region,
            crate::state::stream_arn_of_consumer(&input.consumer_arn),
        )?;
        let consumer = stream.consumer_by_arn(&input.consumer_arn)?;
        consumer
            .subscription_contested(&input.shard_id, established_at)
            .then(|| ResourceInUseException {
                message: Some(crate::state::another_active_subscription_message(
                    &stream.name,
                    &input.shard_id,
                    &consumer.name,
                )),
            })
    }

    /// Release the shard the subscription this request established was holding: its event
    /// stream has ENDED.
    ///
    /// The stream of the subscription this emulator serves ends when the frames of its
    /// response are complete, which is where this is called from. From then on the consumer
    /// holds nothing for it, and the renewal the service documents — call `SubscribeToShard`
    /// again from the `ContinuationSequenceNumber` the last event reported — is served rather
    /// than refused as a contended subscription.
    ///
    /// Only the record THIS call wrote is released: `established_at` is the instant it
    /// recorded, so a subscription a LATER call established for the same consumer and shard —
    /// having taken it over once the documented window passed — keeps holding the shard for
    /// itself. Releasing a subscription nothing holds is nothing to do, which is why a
    /// consumer, stream or shard that has gone away in the meantime is simply not found here.
    fn release(
        store: &mut crate::state::ComAmazonawsKinesisKinesis20131202State,
        region: &str,
        established_at: std::time::SystemTime,
        input: &SubscribeToShardInput,
    ) {
        let stream_key = {
            let Some(stream) = store.stream_by_arn(
                established_at,
                region,
                crate::state::stream_arn_of_consumer(&input.consumer_arn),
            ) else {
                return;
            };
            crate::state::RegionalName::new(&stream.region, &stream.name)
        };
        if let Some(consumer) = store
            .streams
            .get_mut(&stream_key)
            .and_then(|stream| stream.consumer_by_arn_mut(&input.consumer_arn))
        {
            consumer.subscription_ended_at(&input.shard_id, established_at);
        }
    }

    /// The whole body of a stream that carries ONE in-band failure and no event of its own:
    /// the frame a client reads the output shape out of, then the union member the failure
    /// travels as, marked as the exception frame it is.
    ///
    /// Both failures the `SubscribeToShardEventStream` union carries before any record is
    /// delivered are answered through here — the stream's CMK and a contested subscription —
    /// so a client decodes them the same way.
    fn faulted_stream(
        protocol: &dyn ::smithy_server_runtime::ServerHttpProtocol,
        event_type: &str,
        event: &dyn ::aws_smithy_schema::serde::SerializableStruct,
        schema: &::aws_smithy_schema::Schema,
    ) -> Response {
        match frame_empty_initial_response(protocol).and_then(|mut frames| {
            frames.extend(frame_event(protocol, event_type, true, event, schema)?);
            Ok(frames)
        }) {
            Ok(frames) => event_stream_only_response(frames),
            Err(error) => error_response(
                protocol,
                &internal(format!("the event stream could not be framed: {error}")),
            ),
        }
    }

    /// Read the request into the modelled input shape and check it against the model.
    ///
    /// Driven through the service's own protocol, so the body codec is the protocol's, and a
    /// request the model refuses is answered through this service's OWN refusal seam
    /// ([`smithy_server_runtime::Service::override_refusal`], which `src/service.rs` words
    /// through one module) rather than the framework's default envelope — so a bad request to
    /// this hand-authored operation reads exactly as a bad request to any other operation of
    /// this service.
    ///
    /// The refusal is BOXED because a `Response` is large and this is the error half of a
    /// result (clippy's `result_large_err`).
    fn read_input(
        &self,
        svc: &dyn Service,
        req: &Request,
    ) -> Result<SubscribeToShardInput, Box<Response>> {
        // The definition is read, used and dropped inside this statement: `&dyn
        // ErasedServiceDefinition` is not `Sync`, so holding it across an await would make
        // the serving future non-`Send`.
        let read = read_request::<SubscribeToShardInput>(
            svc.definition().protocol(),
            req,
            &SubscribeToShardInput::SCHEMA,
        );
        match read {
            Ok(input) => Ok(input),
            Err(RequestRefusal::Invalid(violations)) => {
                let stated = svc.override_refusal(&Refusal::Invalid(violations.clone()));
                Err(Box::new(svc.definition().refusal_response(
                    ProtocolError::Invalid(violations),
                    stated,
                    req,
                )))
            }
            // A body the protocol's codec could not read at all is not a constraint refusal
            // and has no violation form: it keeps the envelope the framework's own boundary
            // answers one with.
            Err(RequestRefusal::Unreadable(message)) => Err(Box::new(
                svc.definition()
                    .framework_error_response(ProtocolError::Malformed(message), req),
            )),
        }
    }

    /// The whole subscription: the stream's own CMK, then the event the shard's records make
    /// up, framed as this service's event-stream body.
    async fn respond(
        &self,
        svc: &dyn Service,
        ctx: &OperationContext<'_>,
        region: &str,
        now: std::time::SystemTime,
        input: &SubscribeToShardInput,
    ) -> Response {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return error_response(
                svc.definition().protocol(),
                &internal("the stream store is unavailable".to_string()),
            );
        };

        // Who may subscribe at all is decided at the IAM layer, before anything about the
        // consumer or the shard is read — the same gate every other operation of this service
        // is served behind (`crate::service::authorization`), asked here because this
        // hand-authored operation owns its whole request-to-response step and so is not
        // mounted behind that gate's wrapper. The resource is the CONSUMER the request names,
        // which is the resource type this operation addresses.
        if let Some(message) = crate::service::authorization_refusal(
            &*self.engine,
            ctx,
            "SubscribeToShard",
            &input.consumer_arn,
        ) {
            return error_response(
                svc.definition().protocol(),
                &SubscribeToShardErrorModeled::AccessDeniedException(AccessDeniedException {
                    message: Some(message),
                }),
            );
        }

        // The stream's own CMK decides whether its data plane serves this read at all, so the
        // key is resolved against KMS first — exactly as the pull reader resolves it
        // (`crate::ops::get_records`). The store guard is taken only to read which key the
        // subscribed stream carries, because no guard is held across an await; every refusal
        // about the subscription itself is answered by the body below, which reads the store
        // again.
        let encryption_key = {
            let store = state.lock();
            store
                .stream_by_arn(
                    now,
                    region,
                    crate::state::stream_arn_of_consumer(&input.consumer_arn),
                )
                .and_then(|stream| stream.encryption_key().map(str::to_string))
        };
        let mut disabled_key: Option<String> = None;
        if let Some(key_id) = encryption_key {
            if let crate::state::KeyState::Disabled(key_arn) =
                crate::state::key_state(ctx, region, &key_id).await
            {
                disabled_key = Some(key_arn);
            }
        }

        // A stream whose CMK the data plane may no longer use delivers no record. The
        // `SubscribeToShardEventStream` union is where this operation models that failure
        // (`KMSDisabledException` is a member of the union and not one of the operation's own
        // errors), so it travels as an in-band exception frame of the established stream
        // rather than as a refusal of the call.
        if let Some(key_arn) = disabled_key {
            let event = KMSDisabledException {
                message: Some(crate::state::key_disabled_message(&key_arn)),
            };
            return Self::faulted_stream(
                svc.definition().protocol(),
                KMS_DISABLED_EVENT,
                &event,
                &SCHEMA_KMS_DISABLED_EXCEPTION,
            );
        }

        let established = {
            let mut store = state.lock();
            Self::establish(&mut store, region, now, input)
        };
        let protocol = svc.definition().protocol();
        let mut pushed = match established {
            Ok(Established::Holding(pushed)) => pushed,
            // Two subscriptions of one consumer to one shard contend. The conflict is a
            // member of the output UNION, so this call is served and states it in band —
            // "the second call takes over the subscription and the previous connection
            // expires or fails with a ResourceInUseException" — and the stream it contested
            // states the same thing on its own stream below. It delivers no record, so a
            // consumer whose lease is contested is told so instead of being served the
            // shard twice.
            Ok(Established::Contested(event)) => {
                return Self::faulted_stream(
                    protocol,
                    RESOURCE_IN_USE_EVENT,
                    &event,
                    &SCHEMA_RESOURCE_IN_USE_EXCEPTION,
                )
            }
            Err(error) => return error_response(protocol, &error),
        };

        // The body opens with the frame a client reads the OUTPUT shape out of, before any
        // event of the subscription: `SubscribeToShardOutput`'s only member is the stream
        // itself, so that frame carries no member.
        let mut framed = frame_empty_initial_response(protocol);

        // Then the events, for as long as the subscription is LIVE: one straight away, and
        // one every [`PUSH_INTERVAL`] after that while the consumer still holds the shard —
        // the window a later call takes a subscription over after
        // (`crate::state::SUBSCRIPTION_TAKEOVER_WINDOW`), which is as long as this service
        // calls a subscription contended and so as long as it is one being read. Each of
        // those reads the stored shard AGAIN, so a record written while the subscription is
        // being served is delivered by the event after it, and the shard's own end ends the
        // subscription.
        //
        // Both halves of what this body can cost are CONSTANTS of this crate rather than
        // anything a request chooses: how many events it holds is [`MAX_PUSHED_EVENTS`], and
        // how much each of them carries is the per-event ceiling the shared read is given
        // ([`max_event_records`] / [`MAX_EVENT_BYTES`]). The deadline below is MONOTONIC and
        // the count bounds the loop whatever the wall clock does. The wait between two events
        // is the one await point of the loop, so a caller that goes away, a timeout and a
        // shutdown all land while the subscription is being served, and no store guard is held
        // across it.
        let ends_at = tokio::time::Instant::now() + crate::state::SUBSCRIPTION_TAKEOVER_WINDOW;
        let mut measured = crate::state::Measured::new();
        let mut events = 0u32;
        loop {
            if let Ok(frames) = framed.as_mut() {
                match pushed.event.framed(protocol) {
                    Ok(event) => frames.extend(event),
                    Err(error) => framed = Err(error),
                }
            }
            measured.absorb(pushed.measured);
            events = events.saturating_add(1);
            if pushed.event.ends() || framed.is_err() || events >= MAX_PUSHED_EVENTS {
                break;
            }
            if tokio::time::Instant::now() + PUSH_INTERVAL >= ends_at {
                break;
            }
            tokio::time::sleep(PUSH_INTERVAL).await;
            let at = std::time::SystemTime::now();
            let (contested, again) = {
                let store = state.lock();
                (
                    Self::contested(&store, region, at, input, now),
                    Self::pushed_again(&store, region, at, input, &pushed.next),
                )
            };
            // Another subscription of this consumer to this shard was established behind this
            // one while it was being read: the two contend, so this stream states the
            // conflict as the LAST event it carries and ends there rather than going on
            // delivering records a second reader is also being served. The shard it held is
            // released below, so a consumer that re-acquires after the fault is served again.
            if let Some(event) = contested {
                if let Ok(frames) = framed.as_mut() {
                    match frame_event(
                        protocol,
                        RESOURCE_IN_USE_EVENT,
                        true,
                        &event,
                        &SCHEMA_RESOURCE_IN_USE_EXCEPTION,
                    ) {
                        Ok(fault) => frames.extend(fault),
                        Err(error) => framed = Err(error),
                    }
                }
                break;
            }
            match again {
                Some(again) => pushed = again,
                // The stream, the consumer or the shard is gone: the event stream is over,
                // which is what a consumer reading a resource that was removed under it sees.
                None => break,
            }
        }

        // The store guard above is released, so what the subscription's events measured goes
        // into the CloudWatch store now — off the answer, exactly as the pull reader's page is
        // published (`crate::ops::get_records`).
        measured.publish(&*self.engine);

        // The event stream is complete, so the subscription it carried is OVER and the shard
        // it held is released: the next call of this consumer to this shard is the renewal
        // the service documents, not a second reader contending with this one. Only the record
        // THIS call wrote is released — a subscription another call established in the
        // meantime holds the shard for itself.
        {
            let mut store = state.lock();
            Self::release(&mut store, region, now, input);
        }

        match framed {
            Ok(frames) => event_stream_only_response(frames),
            Err(error) => error_response(
                protocol,
                &internal(format!("the event stream could not be framed: {error}")),
            ),
        }
    }
}

impl<Eng: Send + Sync + 'static> service_common::event_stream::EventStreamOperation
    for SubscribeToShard<Eng>
{
    fn serve<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, Response> {
        Box::pin(async move {
            let input = match self.read_input(svc, req) {
                Ok(input) => input,
                Err(refused) => return *refused,
            };
            let region = crate::state::request_region(ctx);
            let now = std::time::SystemTime::now();

            let response = self.respond(svc, ctx, &region, now, &input).await;
            // One access record per request, so a refused subscription is diagnosable. No
            // request payload and no credential reaches it.
            tracing::debug!(
                operation = "SubscribeToShard",
                service = ctx.identity().signing_name(),
                method = ctx.parts().method.as_str(),
                status = response.status().as_u16(),
                "served an event-stream request"
            );
            response
        })
    }
}
