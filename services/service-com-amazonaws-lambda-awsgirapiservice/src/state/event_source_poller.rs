// SPDX-License-Identifier: CC0-1.0

//! The DATA PLANE of an event source mapping on a SHARD source — an Amazon Kinesis data
//! stream or an Amazon DynamoDB table's stream: what such a mapping does with no client
//! action at all.
//!
//! A mapping's control plane (the record, its UUID and ARN, its lifecycle states) lives in
//! [`crate::state`]. This module is the other half for the stream sources whose records sit
//! in SHARDS, as [`crate::state::sqs_poller`] is for a mapping on a queue: once a mapping on
//! one of those streams is `Enabled`, Lambda itself reads that stream's shards and invokes
//! the mapped function with the batch it read — no caller asks it to. A queue needs a reader
//! of its own because what it hands over, and how a reader keeps its place in it, differ: a
//! queue hides a message it hands over for its own visibility timeout, while a shard hands
//! out the records after a POSITION the reader carries. The two shard sources share this
//! reader for the same reason — both are read per shard from a position the mapping carries
//! — and differ only in how their shards are listed and what one record is handed over as,
//! which is what [`kinesis_stream`] and [`dynamodb_stream`] each state for its own source.
//! The rules this reader follows:
//!
//! * WHILE PAUSED, NOTHING IS READ. "When true, the event source mapping is active. When
//!   false, Lambda pauses polling and invocation" (`Enabled`), so a `Disabled` mapping
//!   reads no record and invokes nothing, and the records put to the stream meanwhile stay
//!   in the shard for its retention period.
//! * THE POSITION IS THE MAPPING'S. `StartingPosition` is "The position in a stream from
//!   which to start reading", a property of the mapping fixed when the mapping is created,
//!   so pausing does not move it ([`crate::state::ShardCursor`]): a mapping enabled again
//!   reads on from where it had got to, which for one that read nothing yet is where its
//!   creation fixed it. That is what makes a backlog built up while it was paused
//!   OBSERVABLE rather than skipped.
//! * EACH BATCH READ IS METERED. The age of the newest record of the batch, at the instant
//!   the batch was read off the shard, is published as this function's
//!   `IteratorAge` ([`crate::state::metrics`]) — so a batch that sat in the
//!   shard while the mapping was paused meters that whole wait, and a batch read as its
//!   records arrive meters almost nothing. The age is measured from the WRITE of that record
//!   ([`stamped_with_the_write`]), never from a stamp a source publishes rounded down to a
//!   coarser resolution than the write, which would meter longer than the record ever waited.
//! * AND SO IS WHAT THE PASS DID WITH THE RECORDS, while the mapping's `MetricsConfig` asks
//!   for it — "the metrics you want your event source mapping to produce" — whichever source
//!   the mapping reads: how many records the pass took out of the stream, how many its
//!   `FilterCriteria` rejected and how many an invocation accepted are published per MAPPING
//!   ([`crate::state::metrics::FunctionMetrics::publish_event_counts`]), with a 0-valued poll
//!   count for a minute in which the pass read nothing. Nothing is DELETED from a shard — its
//!   records stay in it for its retention period — so that one count of the family is not a
//!   quantity this source has at all, which is where it differs from a queue's
//!   ([`crate::state::sqs_poller`]).
//! * THE CRITERIA DECIDE WHICH RECORDS ARE HANDED OVER. `FilterCriteria` is "an object that
//!   defines the filter criteria that determine whether Lambda should process an event", so
//!   of the records a pass read off a shard exactly those at least one of the mapping's
//!   patterns matches are handed to the function ([`admitted_of`]); a record none of them
//!   matches is not handed to any invocation. Such a record is still READ, and the poller
//!   itself is what accounts for it, so the shard's position moves past it exactly as it
//!   moves past a record an invocation processed — which is what lets the records behind one
//!   the criteria reject be delivered — and a batch they reject every record of is a pass
//!   that made no invocation at all.
//! * A RECORD PAST THE MAPPING'S RECORD AGE IS DISCARDED. "Discard records older than the
//!   specified age. The default value is infinite (-1)" (`MaximumRecordAgeInSeconds`): a
//!   mapping stating a finite age hands no record older than that to its function
//!   ([`record_age`]), and such a record is accounted for by the pass exactly as one the
//!   criteria rejected — so the shard reads on past it, and a mapping whose whole backlog has
//!   aged out makes no invocation at all while still reading on. A mapping carrying the
//!   infinite default discards nothing.
//! * A MAPPING WITH A TUMBLING WINDOW AGGREGATES OVER IT. "(Kinesis and DynamoDB Streams
//!   only) The duration in seconds of a processing window" (`TumblingWindowInSeconds`): every
//!   invocation of such a mapping carries the window it falls in, the shard and the stream
//!   its records came from and the state that window has reached, the state the handler
//!   returns is what the next invocation of that window is handed, and each window ends in
//!   one final invocation carrying that state ([`tumbling_window`]). What is read, batched
//!   and checkpointed is not changed by a window, but such a mapping reads one shard
//!   SERIALLY ([`in_flight_batches_of`]): the state is the shard's, and it is handed from one
//!   invocation of the window to the next.
//! * A BATCH IS CHECKPOINTED PAST ONLY ONCE DELIVERED. An invocation that did not succeed
//!   leaves the cursor where it was, so the same batch is read again — "The default value
//!   is infinite (-1). When set to infinite (-1), failed records are retried until the
//!   record expires" (`MaximumRetryAttempts`) — and what the attempt came to is reported as
//!   the mapping's own `LastProcessingResult`. A mapping whose `FunctionResponseTypes` asks
//!   for its function's report of which records of the batch failed is checkpointed past
//!   only the records at the head of the batch that report leaves out, so the first record
//!   it names is where the next read starts (see [`super::batch_item_failures`]).
//! * THE RETRIES A MAPPING ALLOWS ARE SPENT ON ONE BATCH. "Discard records after the
//!   specified number of retries" (`MaximumRetryAttempts`): a mapping that states a finite
//!   count gets the first attempt plus that many at one batch, and once they are spent the
//!   shard is read on from PAST those records — so the records behind a batch its function
//!   cannot process are still delivered. The count is a fact about the batch, and about the
//!   attempts the FUNCTION answered for: a pass that could not invoke it at all spends none
//!   of the allowance, and a shard that has moved on starts the count again. Between two
//!   attempts at one batch the wait GROWS
//!   ([`undelivered_backoff`]), so a function that keeps failing is not invoked at the rate
//!   a healthy mapping polls.
//! * A BATCH ITS FUNCTION ERRORED ON IS SPLIT WHERE THE MAPPING ASKS. "If the function
//!   returns an error, split the batch in two before retrying"
//!   (`BisectBatchOnFunctionError`): a mapping that asks for it attempts half of that batch
//!   next, and halves again while the half it attempts is errored on too, down to the single
//!   record — so the records of a batch its function can process are processed and the
//!   record it cannot is attempted, and given up on, on its own
//!   ([`PollerWork::bisect`]). Each half is a batch of its own, so it gets the mapping's own
//!   allowance of attempts. A report that fails the WHOLE batch is such an error; one that
//!   names a suffix of it has already said which records to read again, and is read again as
//!   it stands.
//! * THE BATCH A MAPPING GAVE UP ON IS RECORDED WHERE IT SAYS. The records a mapping
//!   discards are gone from its function's reach, so the only thing a client can still learn
//!   about them is what its `DestinationConfig$OnFailure` names: where it names a
//!   destination, one record describing that batch — not its payloads — is written there
//!   ([`failure_destination`]), and that write is made as the function's execution role like
//!   every other call this poller makes, so a destination whose write the policies in force
//!   on that role withhold receives nothing.
//!
//! * NOTHING IS READ FOR A FUNCTION THAT CANNOT BE INVOKED. "Lambda reads items from the
//!   event source and invokes the function" (`CreateEventSourceMapping`), so a mapping
//!   whose function this store does not hold `Active` takes nothing off a shard — the same
//!   rule [`crate::state::sqs_poller`] holds a queue mapping to.
//! * THE SHARDS ARE READ AS THE FUNCTION'S EXECUTION ROLE. Lambda reads the source acting as
//!   that role, so while the policies in force on it withhold one of the reads consuming the
//!   stream takes, a pass reads nothing and no cursor moves
//!   ([`super::event_source_authorization`]) — the records stay in the shard until the grant
//!   is in place, which is the same question the call that WIRED the pair together was
//!   refused for, and again the rule [`crate::state::sqs_poller`] holds a queue mapping to.
//! * ONE SHARD IS PROCESSED AS MANY BATCHES AT ONCE AS THE MAPPING ASKS FOR.
//!   `ParallelizationFactor` is "the number of batches to process from each shard
//!   concurrently", so a pass takes that many batches off ONE shard and hands them to the
//!   function TOGETHER ([`in_flight_batches_per_shard`]): the invocations really are in
//!   flight at the same instant, which is the only way the setting is observable. A mapping
//!   configuring none processes the one batch per shard it always did. Concurrency is not
//!   allowed to reorder what a client may rely on: the records of one partition key are
//!   handed over in ONE of those batches, in shard order ([`concurrent_batches`]), so only
//!   records of different keys are in flight beside each other. The shard still has ONE
//!   position, so it reads on from past the run of records at the head of what the pass read
//!   that the pass handled — a record a batch did not take, or one its invocation did not
//!   process, is where the next read starts.
//!
//! * A BATCH IS GATHERED OVER THE WINDOW THE MAPPING ASKS FOR. "The maximum amount of time,
//!   in seconds, that Lambda spends gathering records before invoking the function"
//!   (`MaximumBatchingWindowInSeconds`): a mapping stating one hands a batch over once it
//!   holds `BatchSize` records or that span has run since the first record of it was read,
//!   whichever comes first — both are MAXIMA — so such a mapping invokes its function with
//!   what accumulated over the window rather than with each record as it arrives, and a
//!   mapping carrying the stream default of 0 seconds hands over what one read found. The
//!   gathering costs no record and no position: the shard is checkpointed only past a batch
//!   that was delivered, so a batch still gathering is simply read again, with the records
//!   that arrived meanwhile, until it is due ([`PollerWork::gathered_for`]).
//!
//! Every call this module makes goes to the emulator it is running inside, over the
//! in-process callback ([`smithy_server_runtime::EmbeddedEmulator`]): the stream is read
//! through the data-plane surface the source publishes in this same emulator and the
//! function is invoked through its own (`Invoke`), so nothing is read or delivered that this
//! emulator does not serve itself, and a record is consumed only through the data plane a
//! client would consume it through.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ::serde_json::{json, Value};

use super::{
    metrics::{EventCounts, FunctionMetrics},
    ComAmazonawsLambdaAwsgirapiserviceState, ShardCursor, ShardSource,
};

/// How a DynamoDB table's stream is listed, read and handed over.
pub mod dynamodb_stream;
/// What a batch whose retries are spent is recorded as, and where that record goes.
pub mod failure_destination;
/// How a Kinesis data stream is listed, read and handed over.
pub mod kinesis_stream;
/// Which records of a batch are too old for the mapping's own record age to hand over.
mod record_age;
/// What a mapping's `FilterCriteria` admit of one batch, and what each record is matched as.
mod record_filter;
/// The tumbling window a mapping aggregates one shard's records over.
mod tumbling_window;
use record_filter::admitted_of;

/// How long the poller waits before looking at an idle shard again. Short enough that a
/// record put on the stream is delivered promptly, long enough not to spin.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long it waits before looking at a mapping that is NOT reading again — one that
/// rests `Disabled`, or is still settling into the state its `Enabled` setting asks for.
/// Longer than [`POLL_INTERVAL`], because such a mapping has no shard to look at: the one
/// thing a pass does is read the mapping's own state, and nothing but a call can change
/// it.
const PAUSED_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long the poller waits after a failed attempt before trying the batch again. It is
/// multiplied by the number of consecutive failures, up to [`MAX_RETRY_BACKOFF`].
const RETRY_BACKOFF: Duration = Duration::from_millis(250);

/// The longest the poller waits between two attempts at a batch it keeps failing to
/// deliver.
///
/// A mapping whose stream or function cannot be reached keeps trying — that is what lets
/// it recover when the resource comes back — but it does so at this interval rather than
/// at the rate a healthy mapping polls.
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// The longest the poller waits between two attempts at a batch its FUNCTION did not
/// process, which the wait grows towards by doubling ([`undelivered_backoff`]).
///
/// An EMULATOR bound, not an AWS number: what AWS states is that such a batch is retried
/// until it is processed, it expires, or the mapping's `MaximumRetryAttempts` is spent, with
/// the wait between attempts growing. The ceiling is chosen so a function that keeps failing
/// is attempted at a minute scale rather than at the rate a healthy mapping polls, and so a
/// function that starts succeeding again is still tried within a minute.
const MAX_UNDELIVERED_BACKOFF: Duration = Duration::from_secs(60);

/// How often a mapping that keeps failing says so: the first failure, and every this many
/// after it, so one broken mapping cannot fill the log.
const FAILURE_LOG_INTERVAL: u32 = 20;

/// The most records this emulator reads in ONE batch, whatever `BatchSize` the mapping
/// configures.
///
/// This is the EMULATOR's bound, not an AWS number (`BatchSize` itself reaches 10000): a
/// batch is held in memory and the whole of it becomes one invocation payload, so the
/// count a caller chose is clamped before anything is read. A mapping configured above it
/// reads this many records per batch instead, which is a smaller batch and never a dropped
/// record.
const MAX_RECORDS_PER_BATCH: i32 = 1_000;

/// The most records this emulator reads for ONE shard in one pass, whatever `BatchSize` and
/// `ParallelizationFactor` come to together ([`read_window`]).
///
/// This is the EMULATOR's bound, like [`MAX_RECORDS_PER_BATCH`] and for the same reason: every
/// record a pass read is held in memory until the batches it routed them to have been handed
/// over. A mapping whose two settings multiply past it reads fewer batches at once, which is
/// less concurrency and never a dropped record.
const MAX_RECORDS_PER_PASS: usize = 1_000;

/// The most pages of a stream's shard listing one pass walks, so a stream cannot keep the
/// poller in its shard listing.
const MAX_SHARD_PAGES: usize = 10;

/// The batches of ONE shard a mapping that configures no `ParallelizationFactor` has in
/// flight: the one batch per shard this poller has always handed over per pass.
const ONE_BATCH_PER_SHARD: usize = 1;

/// The range `ParallelizationFactor` admits, from the member's own
/// `@range(min: 1, max: 10)`: the value is read inside it however it got into the store.
const MIN_PARALLELIZATION_FACTOR: i32 = 1;
/// See [`MIN_PARALLELIZATION_FACTOR`].
const MAX_PARALLELIZATION_FACTOR: i32 = 10;

/// How many batches of ONE shard a mapping may have in flight at one instant, which is how
/// many invocations of its function a pass over that shard hands over concurrently.
///
/// "(Kinesis and DynamoDB Streams only) The number of batches to process from each shard
/// concurrently" (`ParallelizationFactor`) is what a mapping configuring one asks for: that
/// many batches are read off the shard and handed over together, read inside the range the
/// member admits ([`MIN_PARALLELIZATION_FACTOR`]). A mapping configuring none keeps this
/// poller's own cadence of [`ONE_BATCH_PER_SHARD`], which is what it has always handed over
/// per pass.
///
/// Two further bounds hold whatever a mapping configures, because neither is the mapping's
/// to raise — the function's own reservation and the runs this process can hold at once —
/// and they are the same two a queue source's concurrency is held to, so both sources read
/// them in one place ([`super::sqs_poller::batches_in_flight`]).
///
/// Never zero: a pass always hands over the one batch it would have handed over anyway.
pub(crate) fn in_flight_batches_per_shard(
    parallelization_factor: Option<i32>,
    reserved_concurrent_executions: Option<i32>,
) -> usize {
    let Some(configured) = parallelization_factor else {
        return ONE_BATCH_PER_SHARD;
    };
    super::sqs_poller::batches_in_flight(
        configured.clamp(MIN_PARALLELIZATION_FACTOR, MAX_PARALLELIZATION_FACTOR),
        reserved_concurrent_executions,
    )
}

/// How many batches of ONE shard a mapping has in flight once its TUMBLING WINDOW is taken
/// into account: what [`in_flight_batches_per_shard`] answers, or exactly one while the
/// mapping aggregates over a window.
///
/// A window's state is the SHARD's: the state one invocation of a window returns is what the
/// NEXT invocation of that window is handed ([`tumbling_window`]), which only holds while the
/// invocations of one shard's window are made one after another. Two invocations of the same
/// window in flight beside each other would each be handed the state the other had not
/// returned yet, and one of the two aggregates would be lost — a plausible figure that is not
/// the aggregate over the window. So a mapping that configures a window reads its shard
/// serially, whatever `ParallelizationFactor` it also carries, and the records it reads are
/// unchanged: a pass takes one batch instead of several.
fn in_flight_batches_of(
    tumbling_window: Option<Duration>,
    parallelization_factor: Option<i32>,
    reserved_concurrent_executions: Option<i32>,
) -> usize {
    if tumbling_window.is_some() {
        return ONE_BATCH_PER_SHARD;
    }
    in_flight_batches_per_shard(parallelization_factor, reserved_concurrent_executions)
}

/// Start the in-process poller for the event source mapping `uuid` names, if one is not
/// already running for it.
///
/// Called by an operation that has created or updated the mapping, AFTER it has released
/// the store's lock. It changes nothing a caller is told: the poller observes the
/// mapping's own `State` and reads nothing until the clock has moved it to `Enabled`. With
/// no async runtime to spawn onto there is no poller and nothing is polled.
///
/// At most ONE task runs per mapping, and the store holds at most
/// [`super::MAX_EVENT_SOURCE_MAPPINGS`] mappings, so the tasks this can ever spawn are
/// bounded by that count. A task ends as soon as its mapping is gone or being deleted, so
/// none of them outlives the mapping it polls, and its claim is released however it ends
/// (see [`PollerClaim`]).
pub fn drive_mapping<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &::std::sync::Arc<Eng>,
    uuid: &str,
) {
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    {
        let mut store = state.lock();
        // The sources this module reads are the two whose records sit in shards; a mapping
        // on any other source has no reader here, so no task is started for it at all.
        let reads_shards = store
            .event_source_mappings
            .get(uuid)
            .and_then(|mapping| mapping.event_source_arn.clone())
            .as_deref()
            .and_then(super::event_source_vendor)
            .and_then(ShardSource::of_vendor)
            .is_some();
        if !reads_shards || !store.claim_event_source_poller(uuid) {
            return;
        }
    }
    let callback = ctx.callback().clone();
    // The stores this poller reads and publishes through, resolved once because none of the
    // handles changes while it runs ([`PollerStores`]).
    let stores = PollerStores {
        metrics: FunctionMetrics::new(super::metrics::cloudwatch_state(&**engine)),
        policies: PolicyStores {
            iam: super::event_source_authorization::iam_state(&**engine),
            sources: super::event_source_resource_policy::SourcePolicies::of(&**engine),
        },
        arrivals: super::event_source_arrival::SourceArrivals::of(&**engine),
    };
    let claim = PollerClaim {
        state: ::engine::ServiceState::clone(&state),
        uuid: uuid.to_string(),
        held: true,
    };
    runtime.spawn(async move {
        poll_mapping(&callback, claim, stores).await;
    });
}

/// The engine-resolved stores ONE poller holds for as long as it runs: where the work it does
/// is published, what each pass's reads are decided against, and where the instant a record was
/// written is read from.
///
/// Resolved once per poller, because none of the handles changes while it runs.
struct PollerStores {
    /// The CloudWatch store this mapping's work is published to.
    metrics: FunctionMetrics,
    /// The stores each pass's reads are decided against.
    policies: PolicyStores,
    /// The store the instant a record was WRITTEN is read from.
    arrivals: super::event_source_arrival::SourceArrivals,
}

/// The stores the decision about a pass's reads is made from: the IAM store the invoked
/// function's execution role is resolved against — an engine carrying none shows nothing
/// about the role — and the stores the EVENT SOURCE's own policy lives in, since a stream is
/// fenced by the policy attached to it whoever reads it, this poller included.
#[derive(Clone)]
struct PolicyStores {
    iam: Option<::engine::ServiceState<super::event_source_authorization::IamState>>,
    sources: super::event_source_resource_policy::SourcePolicies,
}

/// The claim of the one poller of a mapping, held for as long as that poller runs.
///
/// It is released by [`Self::release`] inside the critical section that decided the poller
/// is done, so a call that arrives afterwards starts a new poller. The `Drop` is the
/// backstop for every OTHER way the task can end — the runtime shutting down, or a panic
/// inside the poller — because a claim left behind would stop this mapping from ever being
/// polled again.
///
/// Shared with the reader of the other kind of event source ([`super::sqs_poller`]), since
/// "one poller per mapping" is one rule whichever source the mapping names.
pub(crate) struct PollerClaim {
    pub(crate) state: ::engine::ServiceState<ComAmazonawsLambdaAwsgirapiserviceState>,
    pub(crate) uuid: String,
    /// Whether this claim is still the live one. Set to `false` by [`Self::release`], so
    /// the `Drop` cannot release a claim a LATER poller has since taken.
    pub(crate) held: bool,
}

impl PollerClaim {
    /// Release the claim through a store lock the caller already holds.
    pub(crate) fn release(&mut self, store: &mut ComAmazonawsLambdaAwsgirapiserviceState) {
        if self.held {
            store.release_event_source_poller(&self.uuid);
            self.held = false;
        }
    }
}

impl Drop for PollerClaim {
    fn drop(&mut self) {
        if self.held {
            let mut store = self.state.lock();
            store.release_event_source_poller(&self.uuid);
            self.held = false;
        }
    }
}

/// One record of a batch as this reader carries it, whichever source it was read from: how
/// the shard identifies it, when the source stamped it, and the envelope the function is
/// handed for it.
///
/// The envelope is the SOURCE's own form — a Kinesis record and a DynamoDB stream record
/// are handed to a function as different documents — built by the module that read it, so
/// everything the poller itself does (metering the batch, delivering it, checkpointing past
/// what was processed) is the one rule for both sources.
#[derive(Clone)]
struct ShardRecord {
    /// "the sequence number of the data record in the shard": how a report of this source
    /// names one record, and what the mapping's cursor reads on from.
    sequence_number: String,
    /// The stamp the SOURCE published for the record, which is what a report OF the record
    /// carries ([`failure_destination`]) — the member that source's own data plane answers,
    /// at the resolution it answers it at; `None` for a record it reports no instant for.
    arrived_at: Option<::aws_smithy_types::DateTime>,
    /// When the record was WRITTEN to the shard — the one instant its age is measured from,
    /// for the age a pass meters and for the age the mapping discards a record past
    /// ([`stamped_with_the_write`], [`record_age::age_of`]).
    ///
    /// The stamp above is an APPROXIMATION of this instant for a source that publishes it
    /// rounded down to a coarser resolution than the write, so the two are the same instant
    /// for one source and apart by less than that resolution for the other. `None` for a
    /// record no instant could be read for.
    written_at: Option<::aws_smithy_types::DateTime>,
    /// The envelope the function is handed for this record.
    envelope: Value,
}

/// One mapping's configuration as ONE pass reads it: everything the pass needs, taken from
/// the record under the store's lock and never held across a call.
#[derive(Debug)]
struct ShardSourcedMapping {
    /// Which source the shards below are read from.
    source: ShardSource,
    /// The stream the shards are read from.
    stream_arn: String,
    /// The Region that stream lives in, which is the Region it is reached in: a resource
    /// belongs to one Region's namespace.
    region: String,
    /// The function the batch is handed to, as the mapping resolved it.
    function_arn: String,
    /// That function's bare name, which is the dimension its metrics are published per.
    function_name: String,
    /// The execution role the invocation is attributed to, reported to the function as
    /// `invokeIdentityArn`; `None` when this store holds no configuration to read it from.
    invoke_identity_arn: Option<String>,
    /// How many records one batch holds at most, from `BatchSize`, clamped to what this
    /// emulator reads in one go ([`MAX_RECORDS_PER_BATCH`]).
    batch_size: i32,
    /// How long one batch may be GATHERED for before it is handed over, from
    /// `MaximumBatchingWindowInSeconds` ([`super::sqs_poller::batching_window`]); zero for a
    /// mapping stating none, which is the stream default, and which hands over what one read
    /// found.
    batching_window: Duration,
    /// How many batches of ONE shard the mapping has in flight at once, from
    /// `ParallelizationFactor` ([`in_flight_batches_per_shard`]).
    in_flight_batches: usize,
    /// The patterns the mapping's `FilterCriteria` carry, which decide which records of a
    /// batch the function is invoked with ([`admitted_of`]); empty for a mapping carrying no
    /// criteria, which admits every record.
    filters: Vec<String>,
    /// Whether the mapping's `FunctionResponseTypes` asks for the function's report of which
    /// records of the batch failed to be read (see [`super::batch_item_failures`]).
    reports_batch_item_failures: bool,
    /// How many attempts one batch of this mapping gets before its records are discarded,
    /// from `MaximumRetryAttempts`; `None` for the infinite default
    /// ([`attempts_allowed`]).
    attempts_allowed: Option<u32>,
    /// Whether the mapping's `BisectBatchOnFunctionError` asks for a batch its function
    /// errored on to be SPLIT IN TWO before it is attempted again ([`PollerWork::bisect`]).
    bisects_on_function_error: bool,
    /// The destination the mapping's `DestinationConfig$OnFailure` names, when it names one:
    /// where the record of a discarded batch is written ([`failure_destination`]).
    on_failure_destination: Option<String>,
    /// How old a record of this mapping may be and still be handed to the function, from
    /// `MaximumRecordAgeInSeconds`; `None` for the infinite default, which discards no
    /// record at all ([`record_age`]).
    maximum_record_age: Option<Duration>,
    /// The tumbling window this mapping aggregates each shard's records over, from
    /// `TumblingWindowInSeconds`; `None` for a mapping with no window ([`tumbling_window`]).
    tumbling_window: Option<Duration>,
    /// Whether this mapping's `MetricsConfig` asks for the per-mapping EVENT COUNTS — what
    /// each pass did with the records it took out of the stream
    /// ([`super::metrics::publishes_event_counts`]). The switch is the mapping's own and the
    /// quantities are the poller's, whichever kind of source it reads, so a mapping on a
    /// stream meters a pass exactly as one on a queue does ([`super::sqs_poller`]).
    meters_event_counts: bool,
    /// Where the mapping reads next in each shard it has already read from.
    cursors: BTreeMap<String, ShardCursor>,
    /// Where it reads a shard it has not read from yet.
    initial_cursor: ShardCursor,
}

/// What the poller may do on one pass, decided under the store's lock.
enum Pass {
    /// The mapping is `Enabled`: read what its shards hold under this configuration. Boxed
    /// because it is far larger than the other answers, which most passes are.
    Run(Box<ShardSourcedMapping>),
    /// The mapping is not reading: it is still settling into the state its `Enabled`
    /// setting asks for, it rests `Disabled`, which "pauses polling and invocation", or the
    /// function it invokes is not `Active`, so there is no invocation to read a batch for.
    /// Nothing is read and the poller stays, so the mapping resumes where it left off.
    Paused,
    /// The mapping is `Enabled` but names an event source this reader cannot read, and
    /// why. Nothing is read and the poller ends.
    Unserved(String),
    /// The mapping is gone or being deleted. The claim is already released.
    Stop,
}

/// What ONE pass over the mapping's shards came to.
enum Outcome {
    /// Every batch read was delivered, so there may be more waiting.
    Delivered,
    /// Every shard held nothing to read.
    Idle,
    /// A call failed, or an invocation did not succeed, for at least one shard. That
    /// shard was not checkpointed past, so its records are read again.
    Failed {
        /// What went wrong, for the one log line a failing mapping writes.
        detail: String,
        /// Whether what failed was the FUNCTION's processing of a batch that was read and
        /// handed over, rather than a read or a call this pass could not make. The two are
        /// retried on different ladders: a batch whose function keeps failing is a property
        /// of that batch, so the wait between attempts at it grows by doubling
        /// ([`undelivered_backoff`]), while a pass that could not act at all keeps looking
        /// at the interval a mapping waiting on a resource looks at ([`retry_backoff`]).
        undelivered: bool,
    },
}

/// What ONE attempt at one batch came to.
struct Attempt {
    /// The records of the batch the attempt PROCESSED, in the order the batch holds them.
    processed: Vec<String>,
    /// What the invocation itself answered, and `None` when no invocation was made at all.
    answered: Option<Answered>,
    /// Why the attempt processed no record of the batch, when it processed none.
    failure: Option<String>,
}

/// What one invocation of the mapped function reported about the batch it was handed.
///
/// Every member is the invocation's own answer (`InvocationResponse`), kept because the
/// record of a DISCARDED batch reports what the last attempt at it came to
/// ([`failure_destination`]).
pub(crate) struct Answered {
    /// `StatusCode`: "The HTTP status code is in the 200 range for a successful request."
    pub(crate) status_code: Option<i32>,
    /// `ExecutedVersion`: "The version of the function that executed."
    pub(crate) executed_version: Option<String>,
    /// `FunctionError`: "If present, indicates that an error occurred during function
    /// execution."
    pub(crate) function_error: Option<String>,
    /// The id of the invocation request this answer answers, as the answer carried it.
    pub(crate) request_id: Option<String>,
    /// `Payload`: "The response from the function, or an error object."
    pub(crate) payload: Option<Vec<u8>>,
}

/// Poll one mapping until it is gone.
///
/// The store's lock is taken for the decision of each pass and released before any call
/// leaves this process, so a request never waits on a delivery.
async fn poll_mapping(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    mut claim: PollerClaim,
    stores: PollerStores,
) {
    let uuid = claim.uuid.clone();
    // A handle of its own, so the store can be locked while the claim is borrowed.
    let state = ::engine::ServiceState::clone(&claim.state);
    let mut work = PollerWork::default();
    let mut failures = 0u32;
    loop {
        let pass = {
            let mut store = state.lock();
            // The state moves the clock has ALREADY made due, so a mapping whose
            // `Creating` span has lapsed is seen as the `Enabled` mapping it became.
            store.materialize_all(SystemTime::now());
            next_pass(&mut store, &mut claim)
        };
        match pass {
            Pass::Stop => return,
            Pass::Paused => ::tokio::time::sleep(PAUSED_POLL_INTERVAL).await,
            Pass::Unserved(detail) => {
                ::tracing::warn!(
                    mapping = %uuid,
                    detail = %detail,
                    "this emulator reads no event source of that kind, so the mapping's \
                     source is left untouched"
                );
                let mut store = state.lock();
                claim.release(&mut store);
                return;
            }
            Pass::Run(run) => {
                // Lambda reads the shards ACTING AS the function's execution role, so this
                // pass's reads are subject to the policies in force on that role AT THIS
                // INSTANT rather than when the pair was wired: while one of them is
                // withheld, nothing is read off a shard — no record is delivered and no
                // cursor moves — and the mapping resumes where it left off once the grant is
                // back (see [`super::event_source_authorization`], including the cases that
                // show nothing about a role and withhold nothing).
                if let Some(detail) =
                    withheld_shard_read(&stores.policies, &run, SystemTime::now())
                {
                    failures = failures.saturating_add(1);
                    if failures == 1 || failures.is_multiple_of(FAILURE_LOG_INTERVAL) {
                        ::tracing::warn!(
                            mapping = %uuid,
                            attempt = failures,
                            detail = %detail,
                            "an event source mapping may not read its stream as its \
                             function's execution role; the records stay in the shard until \
                             that role is granted the reads"
                        );
                    }
                    ::tokio::time::sleep(retry_backoff(failures)).await;
                    continue;
                }
                match move_batches(callback, &mut work, &state, &stores, &uuid, &run).await
                {
                    Outcome::Delivered => failures = 0,
                    Outcome::Idle => {
                        failures = 0;
                        ::tokio::time::sleep(POLL_INTERVAL).await;
                    }
                    Outcome::Failed {
                        detail,
                        undelivered,
                    } => {
                        failures = failures.saturating_add(1);
                        if failures == 1 || failures.is_multiple_of(FAILURE_LOG_INTERVAL) {
                            ::tracing::warn!(
                                mapping = %uuid,
                                attempt = failures,
                                detail = %detail,
                                "an event source mapping could not move a batch from its \
                                 stream to its function; the records stay in the shard and \
                                 it tries again"
                            );
                        }
                        let waiting = if undelivered {
                            undelivered_backoff(failures)
                        } else {
                            retry_backoff(failures)
                        };
                        ::tokio::time::sleep(waiting).await;
                    }
                }
            }
        }
    }
}

/// Why the reads this pass would make on `run`'s stream are WITHHELD — from the execution
/// role of the function the mapping invokes, or by the policy attached to the stream itself
/// — or `None` when they are not, which includes every case that shows nothing about either
/// (see [`super::event_source_authorization`]).
///
/// Only the IAM store and the source's own are locked here, one at a time, and this
/// service's is not: the decision is taken between two passes rather than inside the
/// critical section either of them is decided in.
fn withheld_shard_read(
    policies: &PolicyStores,
    run: &ShardSourcedMapping,
    now: SystemTime,
) -> Option<String> {
    let role_arn = run.invoke_identity_arn.as_deref()?;
    super::event_source_authorization::unreadable_event_source_held(
        policies.iam.as_ref(),
        &policies.sources,
        role_arn,
        &run.stream_arn,
        now,
    )
}

/// How long to wait after `failures` consecutive failed attempts at one batch: it grows
/// with them, capped at [`MAX_RETRY_BACKOFF`].
fn retry_backoff(failures: u32) -> Duration {
    RETRY_BACKOFF
        .saturating_mul(failures.max(1))
        .min(MAX_RETRY_BACKOFF)
}

/// How long to wait after `failures` consecutive attempts at a batch the FUNCTION did not
/// process: it DOUBLES with each of them, capped at [`MAX_UNDELIVERED_BACKOFF`].
///
/// "When set to infinite (-1), failed records are retried until the record expires"
/// (`MaximumRetryAttempts`), so such a batch is attempted again however many times it has
/// already failed — but the wait between two attempts grows with them, so a function that
/// keeps failing one batch is not invoked at the rate a healthy mapping polls and the
/// failures it reports do not grow without bound over the span a caller watches them.
fn undelivered_backoff(failures: u32) -> Duration {
    let doubling = 1u32.checked_shl(failures.saturating_sub(1).min(u32::BITS - 1));
    match doubling {
        Some(factor) => RETRY_BACKOFF
            .saturating_mul(factor)
            .min(MAX_UNDELIVERED_BACKOFF),
        // Past what a doubling fits in: the wait has long since reached its ceiling.
        None => MAX_UNDELIVERED_BACKOFF,
    }
}

/// The WORKING state one poller task carries from pass to pass: how it reaches the rest of
/// this emulator, and what it has already tried.
///
/// It belongs to the task rather than to the store because every part of it is a fact about
/// the attempts THIS poller has made, not one a caller reads off the mapping.
#[derive(Default)]
struct PollerWork {
    /// The clients this data plane reaches the other services of this emulator through, built
    /// on first use and then reused.
    clients: Clients,
    /// How many attempts the batch at the head of each shard has had, keyed by shard id, so
    /// the mapping's `MaximumRetryAttempts` is spent on ONE batch. Bounded by the shards of
    /// the one stream this poller reads, and an entry is given up as soon as its shard moves
    /// on.
    attempts: BTreeMap<String, BatchAttempts>,
    /// How many records the next batch of a shard holds while a batch its function errored
    /// on is being SPLIT, keyed by shard id ([`BisectedBatch`]). Bounded and given up like
    /// [`Self::attempts`], and empty for a mapping that does not ask for the split.
    bisected: BTreeMap<String, BisectedBatch>,
    /// The tumbling window each shard is aggregating into, keyed by shard id
    /// ([`tumbling_window`]). Bounded like [`Self::attempts`] by the shards of the one
    /// stream this poller reads, an entry is given up as soon as its window is finalised,
    /// and the map is empty for a mapping that configures no window.
    windows: BTreeMap<String, tumbling_window::OpenWindow>,
    /// When the batch at the head of each shard was first read, keyed by shard id, which is
    /// when that batch's batching window opened ([`GatheringBatch`]). Bounded and given up
    /// like [`Self::attempts`], and never consulted for a mapping whose batching window is
    /// the stream default of zero.
    gathering: BTreeMap<String, GatheringBatch>,
    /// The minute this mapping last published a 0-valued poll count for, for a mapping that
    /// meters its passes: such a zero says the poller looked and found nothing, and it is
    /// published once per minute, which is the resolution the series is aggregated at
    /// ([`claims_idle_minute`]). `None` while the poller has published none.
    idle_minute: Option<i64>,
}

/// When the batch at the head of ONE shard was first read, and which batch that is.
///
/// "The maximum amount of time, in seconds, that Lambda spends gathering records before
/// invoking the function" (`MaximumBatchingWindowInSeconds`) is measured from the first
/// record of the batch being gathered, so the instant belongs to the records it was opened
/// for: it is keyed by the position that batch starts at and given up as soon as the shard
/// has moved on, because a batch read from a moved position is a different batch and gathers
/// over its own window.
#[derive(Debug)]
struct GatheringBatch {
    /// The sequence number of the first record of the batch being gathered, which is the
    /// position the shard is still at.
    head: String,
    /// When that record was first read, which is when this batch's window opened.
    opened_at: SystemTime,
}

/// How many records one batch of ONE shard holds while the mapping is splitting a batch its
/// function errored on, and which batch that is.
///
/// "BisectBatchOnFunctionError – If the function returns an error, split the batch in two
/// before retrying": the split is a fact about the batch the error was answered for, so it
/// is keyed by the position that batch starts at and is given up as soon as the shard has
/// moved on — a batch read from a moved position is a different batch and is read at the
/// mapping's own `BatchSize` again.
#[derive(Debug)]
struct BisectedBatch {
    /// The sequence number of the first record of the batch that was split, which is the
    /// position the shard is still at.
    head: String,
    /// How many records each half of it holds.
    half: usize,
}

impl PollerWork {
    /// How many records one batch read at `head` of `shard` holds: the `configured` size, or
    /// the half a batch the function errored on was split into, while the shard is still at
    /// that batch's own position.
    fn batch_limit(&self, shard: &str, head: &str, configured: usize) -> usize {
        match self.bisected.get(shard) {
            Some(held) if held.head == head => held.half.clamp(1, configured.max(1)),
            _ => configured,
        }
    }

    /// Record that the batch of `held` records at `head` of `shard` is to be SPLIT before it
    /// is attempted again, and answer how many records each half holds.
    ///
    /// A split batch is a DIFFERENT batch — it holds other records than the one the error was
    /// answered for — so the attempts the mapping allows one batch start again for it.
    /// `MaximumRetryAttempts` is spent on a batch ("Discard records after the specified number
    /// of retries"), and the halves are not that batch; reading the allowance as one spent
    /// across a batch and the halves it is split into would make the setting a mapping
    /// enabled do nothing at all wherever it allows no retry, and would leave the one record
    /// its function cannot process neither isolated nor recorded at its failure destination —
    /// which is what splitting the batch is for. So a mapping that allows no retry still
    /// attempts each half once, and the record that survives the halving is the one that is
    /// discarded and recorded, rather than the whole batch it arrived in.
    fn bisect(&mut self, shard: &str, head: &str, held: usize) -> usize {
        let half = held.div_ceil(2).max(1);
        self.bisected.insert(
            shard.to_string(),
            BisectedBatch {
                head: head.to_string(),
                half,
            },
        );
        self.attempts.remove(shard);
        half
    }

    /// How long the batch at `head` of `shard` has been GATHERING at `now`, opening its
    /// window at this instant for a batch not seen at that position before.
    ///
    /// The window is measured from the first record of the batch, not from the pass that
    /// found the shard empty, so it opens when there is something to gather.
    fn gathered_for(&mut self, shard: &str, head: &str, now: SystemTime) -> Duration {
        let opened_at = match self.gathering.get(shard) {
            Some(held) if held.head == head => held.opened_at,
            _ => {
                self.gathering.insert(
                    shard.to_string(),
                    GatheringBatch {
                        head: head.to_string(),
                        opened_at: now,
                    },
                );
                now
            }
        };
        now.duration_since(opened_at).unwrap_or_default()
    }

    /// The shard has MOVED ON, so neither the attempts spent on the batch it was at nor the
    /// split that batch was being read in says anything about what it reads next, and the
    /// window the batch it handed over was gathered over is over with it.
    fn moved_on(&mut self, shard: &str) {
        self.attempts.remove(shard);
        self.bisected.remove(shard);
        self.gathering.remove(shard);
    }
}

/// How many consecutive attempts the batch at the head of ONE shard has had, and which batch
/// those attempts were at.
///
/// `MaximumRetryAttempts` is spent on a BATCH — "Discard records after the specified number
/// of retries" — so the count belongs to the records the attempts were made with and starts
/// again once the shard has moved on to others.
#[derive(Debug)]
struct BatchAttempts {
    /// The sequence number of the FIRST record of the batch, which is the position the shard
    /// is still at: a batch read from a moved position is a different batch.
    head: String,
    /// How many attempts at that batch have been made, the first one included.
    made: u32,
}

/// How many attempts one batch of a mapping carrying `maximum_retry_attempts` gets: the
/// first attempt plus the retries that member allows.
///
/// `None` is "as many as it takes": "Discard records after the specified number of retries.
/// The default value is infinite (-1). When set to infinite (-1), failed records are retried
/// until the record expires" (`MaximumRetryAttempts`), which is what a mapping carrying no
/// value and one carrying the infinite default both ask for.
fn attempts_allowed(maximum_retry_attempts: Option<i32>) -> Option<u32> {
    // A negative value is the infinite default; every other value the member's own range
    // admits is a count of retries.
    let retries = u32::try_from(maximum_retry_attempts?).ok()?;
    Some(retries.saturating_add(1))
}

/// What the poller does next for the mapping it claims, releasing its claim when the
/// answer is [`Pass::Stop`] — the release and the state it was decided from are read in ONE
/// critical section, so a call that has already re-created a poller's work either keeps
/// this poller or starts a new one, never neither.
fn next_pass(
    store: &mut ComAmazonawsLambdaAwsgirapiserviceState,
    claim: &mut PollerClaim,
) -> Pass {
    let resolved = match store.event_source_mappings.get(&claim.uuid) {
        // "When you delete an event source mapping, it enters a Deleting state": nothing
        // more is read for it, and its removal is owed by the clock.
        None => None,
        Some(mapping) if mapping.is_deleting() => None,
        // Still moving into the state its settings ask for, or resting `Disabled`.
        Some(mapping)
            if mapping.is_settling()
                || mapping.state != super::EVENT_SOURCE_MAPPING_STATE_ENABLED =>
        {
            Some(Pass::Paused)
        }
        // "Lambda reads items from the event source and invokes the function": a mapping
        // whose function this store does not hold `Active` takes nothing off a shard, so
        // no batch is read for an invocation that could not be made.
        Some(mapping)
            if !store
                .functions
                .get(&mapping.function_name)
                .is_some_and(|function| function.state == super::STATE_ACTIVE) =>
        {
            Some(Pass::Paused)
        }
        Some(mapping) => Some(match run_of(store, mapping) {
            Ok(run) => Pass::Run(Box::new(run)),
            // A source this reader cannot read is reported by the pass itself, so the one
            // log line is written outside the lock.
            Err(detail) => Pass::Unserved(detail),
        }),
    };
    match resolved {
        Some(pass) => pass,
        None => {
            claim.release(store);
            Pass::Stop
        }
    }
}

/// The configuration one pass reads under, or why this mapping's source cannot be read
/// here.
fn run_of(
    store: &ComAmazonawsLambdaAwsgirapiserviceState,
    mapping: &super::EventSourceMappingRecord,
) -> Result<ShardSourcedMapping, String> {
    let Some(stream_arn) = mapping.event_source_arn.clone() else {
        return Err("the mapping names no event source ARN to read".to_string());
    };
    let Some(parsed) = ::service_common::arn::parse(&stream_arn) else {
        return Err("the mapping's event source is not a resource ARN".to_string());
    };
    let Some(source) = ShardSource::of_vendor(parsed.vendor) else {
        return Err(format!(
            "an event source of the {} namespace has no reader in this emulator",
            parsed.vendor
        ));
    };
    let region = parsed.region.to_string();
    // "The duration in seconds of a processing window" (`TumblingWindowInSeconds`), for a
    // mapping that states one ([`tumbling_window`]). Read before the batches in flight,
    // because a window decides how many of them one shard may have
    // ([`in_flight_batches_of`]).
    let tumbling_window = tumbling_window::tumbling_window(mapping.tumbling_window_in_seconds);
    // The role the invocation is attributed to, read from the VERSION this mapping
    // invokes: "The ARN of the Lambda function" the mapping carries may be qualified, and
    // an alias names the version it invokes.
    let invoke_identity_arn = store.functions.get(&mapping.function_name).and_then(|function| {
        let version = mapping
            .function_qualifier
            .as_deref()
            .and_then(|qualifier| function.resolved_version(qualifier))
            .unwrap_or_else(|| super::VERSION_LATEST.to_string());
        function.config_of(&version).map(|config| config.role.clone())
    });
    Ok(ShardSourcedMapping {
        source,
        stream_arn,
        region,
        function_arn: mapping.function_arn.clone(),
        function_name: mapping.function_name.clone(),
        invoke_identity_arn,
        batch_size: mapping.batch_size.clamp(1, MAX_RECORDS_PER_BATCH),
        // "The maximum amount of time, in seconds, that Lambda spends gathering records
        // before invoking the function": the one rule whichever source the mapping polls,
        // so it is read where the queue reader reads it.
        batching_window: super::sqs_poller::batching_window(
            mapping.maximum_batching_window_in_seconds,
        ),
        // "The number of batches to process from each shard concurrently", held to what the
        // function's own concurrency admits and to what the mapping's own tumbling window
        // admits ([`in_flight_batches_of`]).
        in_flight_batches: in_flight_batches_of(
            tumbling_window,
            mapping.parallelization_factor,
            store
                .functions
                .get(&mapping.function_name)
                .and_then(|function| function.reserved_concurrent_executions),
        ),
        meters_event_counts: super::metrics::publishes_event_counts(
            mapping.metrics_config.as_ref(),
        ),
        filters: super::event_source_filter::patterns_of(mapping.filter_criteria.as_ref()),
        reports_batch_item_failures: super::reports_batch_item_failures(
            &mapping.function_response_types,
        ),
        attempts_allowed: attempts_allowed(mapping.maximum_retry_attempts),
        // "If the function returns an error, split the batch in two before retrying"
        // (`BisectBatchOnFunctionError`), for a mapping that asks for it.
        bisects_on_function_error: mapping.bisect_batch_on_function_error.unwrap_or(false),
        on_failure_destination: failure_destination::on_failure_destination(
            mapping.destination_config.as_ref(),
        ),
        // "Discard records older than the specified age" (`MaximumRecordAgeInSeconds`),
        // for a mapping that states a finite one ([`record_age`]).
        maximum_record_age: record_age::maximum_record_age(mapping.maximum_record_age_in_seconds),
        tumbling_window,
        cursors: mapping.shard_cursors.clone(),
        initial_cursor: mapping.initial_shard_cursor(),
    })
}

/// Read what each shard of the mapped stream holds and hand it to the function.
///
/// Each shard is read on its own — a shard's position is its own — so a batch one shard
/// could not deliver does not stop the others from being read on this pass. What ONE shard
/// yields is as many batches as the mapping may have in flight for it, "the number of
/// batches to process from each shard concurrently" (`ParallelizationFactor`): they are read
/// one after another from the position the shard is at ([`read_window`]) and handed to the
/// function TOGETHER ([`concurrent_batches`]), so a mapping configured for several of them
/// really has several invocations running at the same instant.
///
/// Every batch read is metered before anything is handed over, because the age it meters is a
/// property of the READ: it is how long the newest record of the batch had been in the
/// shard at the instant Lambda took it off, whatever the invocation then comes to.
///
/// `work` carries this poller's own working state: the clients this data plane reaches the
/// rest of the emulator through, and how many attempts the batch at the head of each shard
/// has already had — so a batch the function keeps failing is discarded once the mapping's
/// `MaximumRetryAttempts` is spent on it ([`failed_attempt`]), and the record of the batch it
/// gave up on is written where the mapping says ([`failure_destination`]).
async fn move_batches(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    work: &mut PollerWork,
    state: &::engine::ServiceState<ComAmazonawsLambdaAwsgirapiserviceState>,
    stores: &PollerStores,
    uuid: &str,
    run: &ShardSourcedMapping,
) -> Outcome {
    let listed = match shard_ids(callback, &mut work.clients, run).await {
        Ok(listed) => listed,
        Err(detail) => {
            return Outcome::Failed {
                detail,
                undelivered: false,
            }
        }
    };
    let mut delivered = false;
    let mut failure: Option<String> = None;
    // What this pass did with the records it takes out of the stream, for a mapping that
    // meters its passes: the quantities are counted as the pass does the work and published
    // ONCE for the whole pass below, so a reader summing a period over any of the series
    // gets the records of that period.
    let mut counts = EventCounts::default();
    // The criteria this pass decides each record it reads by, read once for the pass rather
    // than per shard: the patterns are the mapping's and do not change while it runs.
    let criteria = super::event_source_filter::Filters::of(uuid, &run.filters);
    // Whether what failed was the FUNCTION's processing of a batch this pass handed over,
    // rather than something the pass could not do at all (see [`Outcome::Failed`]).
    let mut undelivered = false;
    for shard in &listed.shards {
        let shard = shard.as_str();
        // A tumbling window the clock has ENDED is finalised before anything more is read
        // off this shard, so the records that follow aggregate into the window the clock is
        // in rather than into the one that is over ([`close_due_window`]).
        if let Err(detail) = close_due_window(callback, work, run, shard, SystemTime::now()).await {
            failure = Some(detail);
            undelivered = true;
            continue;
        }
        let cursor = run
            .cursors
            .get(shard)
            .cloned()
            .unwrap_or_else(|| run.initial_cursor.clone());
        let read = match read_window(
            callback,
            &mut work.clients,
            &stores.arrivals,
            run,
            shard,
            &cursor,
        )
        .await
        {
            // "If the shard has been closed, the shard iterator can't return more data":
            // such a shard has no position left to read from.
            Ok(None) => continue,
            Ok(Some(read)) => read,
            Err(detail) => {
                failure = Some(detail);
                continue;
            }
        };
        // The records the pass read off this shard, in the order the shard holds them: the
        // batches it read, one after the other.
        let records: Vec<ShardRecord> = read.concat();
        if records.is_empty() {
            continue;
        }
        // The grant is asked about once more now that the records are in hand, because the
        // policies in force on the role decide this pass at the instant it acts rather than
        // at the instant it started: a grant taken away while this pass was reading means
        // nothing is handed over, nothing is metered and the shard's position does not move,
        // so the records stay for the pass that follows the grant coming back.
        if let Some(detail) = withheld_shard_read(&stores.policies, run, SystemTime::now()) {
            failure = Some(detail);
            continue;
        }
        // Every record read, by the identifier a report of this source names one by:
        // "the sequence number of the data record in the shard".
        let sequence_numbers: Vec<String> = records
            .iter()
            .map(|record| record.sequence_number.clone())
            .collect();
        // The position the shard is at, which is the record the batch at its head starts
        // with: what the attempts spent on that batch, and a split of it, are keyed by.
        let position: &str = sequence_numbers.first().map_or("", String::as_str);
        // How many records ONE batch of this mapping holds here: its `BatchSize`, or the half
        // a batch its function errored on was split into, while the shard is still at that
        // batch's own position ([`PollerWork::batch_limit`]).
        let limit = work.batch_limit(shard, position, batch_records(run));
        // "The maximum amount of time, in seconds, that Lambda spends gathering records
        // before invoking the function" (`MaximumBatchingWindowInSeconds`): a mapping that
        // states a window holds what it read until that batch is full or the window has run,
        // whichever comes first ([`super::sqs_poller::batch_is_due`], the one place that
        // decision is taken whichever source the mapping polls). Gathering costs neither a
        // record nor a position: the cursor moves only past a batch that was DELIVERED, so
        // the pass that follows reads these records again together with whatever arrived
        // meanwhile, and nothing is metered, filtered or handed over for a batch that is
        // still gathering.
        if !run.batching_window.is_zero()
            && !super::sqs_poller::batch_is_due(
                records.len(),
                limit,
                work.gathered_for(shard, position, SystemTime::now()),
                run.batching_window,
            )
        {
            continue;
        }
        // One age per BATCH read, because the age is a property of that read: a pass that
        // took several batches off one shard meters each of them, from the instant its newest
        // record was WRITTEN ([`stamped_with_the_write`]).
        for batch in &read {
            if let Some(last) = batch.last() {
                stores.metrics.publish_iterator_age(
                    &run.function_name,
                    iterator_age_millis(last.written_at.as_ref(), SystemTime::now()),
                );
            }
        }
        // What the mapping's own record age discards of what was read: "Discard records
        // older than the specified age" (`MaximumRecordAgeInSeconds`), decided at the
        // instant the batch was read, so a record past that age is handed to no invocation
        // and the pass itself accounts for it ([`record_age`]).
        let aged_out = record_age::aged_out(run.maximum_record_age, &records, SystemTime::now());
        // What the mapping's criteria admit of the records it did not discard, decided
        // before anything is handed over: a record no pattern matches is not handed to the
        // function at all, and the shard still reads on past it ([`admitted_of`]).
        let admitted = admitted_of(&criteria, run, &records, &aged_out);
        // What the pass took out of this shard, and what the mapping's criteria rejected of
        // it: the records are in hand, so both are counted whatever the hand-over then comes
        // to. A record the pass discarded for a reason of its own — one past the mapping's
        // record age — is no part of what the criteria rejected ([`Admitted::filtered_out`]).
        counts.polled = counts.polled.saturating_add(count_of(records.len()));
        counts.filtered_out = counts
            .filtered_out
            .saturating_add(count_of(admitted.filtered_out(&aged_out)));
        let Some(invoking) = admitted.invoking() else {
            // The pass handed nothing over for what it read — its criteria admitted none of
            // it, or the mapping's record age discarded all of it — so there is no
            // invocation to make for it: the pass handled it all by discarding it, which is
            // a pass that came to no problem, and the shard reads on from past those records
            // so the ones behind them are delivered.
            checkpoint(
                state,
                uuid,
                run,
                shard,
                sequence_numbers.last().cloned(),
                super::LAST_PROCESSING_RESULT_OK,
            );
            work.moved_on(shard);
            delivered = true;
            continue;
        };
        // The batches of this shard that are in flight together, each holding at most the
        // records one batch of this mapping holds — which is the half its function's error
        // split the batch at this position into, while one is being split
        // ([`PollerWork::batch_limit`]).
        let batches = concurrent_batches(
            invoking,
            &grouping_keys(run, listed.partition_key_attribute.as_deref(), invoking),
            run.in_flight_batches,
            limit,
        );
        // The tumbling window this pass's invocations aggregate into, for a mapping that
        // configures one: the window the clock is in, opened for a shard aggregating into
        // none yet, which every invocation of it carries the bounds, the shard, the stream
        // and the state of ([`tumbling_window`]).
        let window = run.tumbling_window.map(|duration| {
            tumbling_window::open(&mut work.windows, shard, duration, SystemTime::now())
        });
        // What each of them is handed over as, and the records it is the function's report
        // about ([`super::batch_item_failures`]): a function can only report about the
        // records it was HANDED.
        let handing: Vec<(Value, Vec<String>)> = batches
            .iter()
            .map(|batch| {
                let mut payload = batch_event(batch);
                if let Some(window) = window.as_ref() {
                    tumbling_window::attach(&mut payload, &run.stream_arn, shard, window, false);
                }
                (
                    payload,
                    batch
                        .iter()
                        .map(|record| record.sequence_number.clone())
                        .collect(),
                )
            })
            .collect();
        // The client every attempt of this pass is made through, resolved ONCE: the
        // invocations below are in flight together, so none of them may hold the working
        // state the client is built on.
        let client = work.clients.lambda(callback, &run.region).clone();
        // CONCURRENTLY: each batch this shard yielded is handed to the function while the
        // others are, which is what `ParallelizationFactor` asks for.
        let attempt = merged_attempt(
            ::futures_util::future::join_all(
                handing
                    .iter()
                    .map(|(payload, invoked)| hand_over_batch(&client, run, payload, invoked)),
            )
            .await,
        );
        // The records the invocations of this shard ACCEPTED, which is what the mapping's
        // invoked count meters: a record a batch was handed but whose invocation did not
        // process it is read again on a later pass, and counted by the pass that gets it
        // through.
        counts.invoked = counts
            .invoked
            .saturating_add(count_of(attempt.processed.len()));
        // What an invocation of a TUMBLING WINDOW returned as its state is what the next
        // invocation of that window is handed, and a state that outgrew its bound ends the
        // window at once ([`tumbling_window::carry_state`]). Only an attempt the function
        // processed the batch in carries its state on: a batch that is to be attempted again
        // is aggregated once, when it is processed.
        if window.is_some() && attempt.failure.is_none() {
            if let Some(answered) = attempt.answered.as_ref() {
                tumbling_window::carry_state(
                    &mut work.windows,
                    shard,
                    answered.payload.as_deref(),
                );
            }
        }
        // The mapping reads on from past the records the pass processed, and the attempt at
        // the head of what it read is reported as what the mapping last came to. A shard has
        // ONE position, so what it reads on from is the run of records at the head that were
        // processed: the first record its function reported it did not process — or one no
        // batch of this pass took — is where the next read starts, whatever became of the
        // records behind that one. A record the pass itself discarded — one the criteria
        // REJECTED, or one past the mapping's record age — is consumed by the pass, so it
        // counts as handled for that position.
        let through = attempt
            .failure
            .is_none()
            .then(|| {
                checkpointed_through(&sequence_numbers, &admitted.handled(&attempt.processed))
            })
            .flatten();
        if let Some(sequence_number) = through {
            checkpoint(
                state,
                uuid,
                run,
                shard,
                Some(sequence_number),
                super::LAST_PROCESSING_RESULT_OK,
            );
            // The shard has moved on, so the attempts the batch it was at had are spent on
            // nothing further, and a split it was being read in is over: the next batch
            // starts its own count at the mapping's own `BatchSize`.
            work.moved_on(shard);
            delivered = true;
            continue;
        }
        // Nothing at the head was processed: the cursor stays where it was, so those records
        // are read again — until the mapping's `MaximumRetryAttempts` is spent on them — and
        // the failed attempt is what the mapping last came to. Each shard is read on its own,
        // so the other shards of the stream are still read on this pass.
        let detail = attempt.failure.clone().unwrap_or_else(|| {
            format!(
                "{} reported the first record of the batch it was handed as one it did not \
                 process",
                run.function_arn
            )
        });
        // The batch the attempts are spent on, and whose records are given up on once they
        // are: the one holding the shard's own next record, which is the batch the position
        // waits on.
        let given_up = batches.first().map_or(&[][..], Vec::as_slice);
        let given_up_sequence_numbers = handing
            .first()
            .map_or(&[][..], |(_, invoked)| invoked.as_slice());
        // "BisectBatchOnFunctionError – If the function returns an error, split the batch in
        // two before retrying": a mapping that asks for it attempts HALF of the batch its
        // function answered the error for next, and halves again while the half it attempts
        // is answered with one too, down to the single record — so the records of a batch its
        // function can process are processed, and the record it cannot is attempted on its
        // own. Only an ANSWERED failure of the whole batch splits it: a pass that could not
        // invoke the function at all learnt nothing about the records, and a function that
        // reported a SUFFIX of the batch as failed has already said which records to read
        // again, so what it reported is read again as it stands
        // ([`super::batch_item_failures`]).
        if run.bisects_on_function_error
            && attempt.failure.is_some()
            && attempt.answered.is_some()
            && given_up_sequence_numbers.len() > 1
        {
            let half = work.bisect(shard, position, given_up_sequence_numbers.len());
            checkpoint(
                state,
                uuid,
                run,
                shard,
                None,
                super::LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED,
            );
            failure = Some(format!(
                "{detail}, so {shard} is read again in batches of {half} record(s)"
            ));
            undelivered = true;
            continue;
        }
        // Where the shard reads on from once that batch is given up on: past the run of
        // records at the head that are accounted for — what this pass handled, plus the
        // records of the batch it gave up on — so a record no batch of this pass was handed
        // is never read past.
        let accounted: Vec<String> = admitted
            .handled(&attempt.processed)
            .into_iter()
            .chain(given_up_sequence_numbers.iter().cloned())
            .collect();
        let failed = failed_attempt(
            state,
            uuid,
            run,
            shard,
            &GivenUpBatch {
                sequence_numbers: given_up_sequence_numbers,
                discarded_through: checkpointed_through(&sequence_numbers, &accounted).as_deref(),
                // An invocation that was not made at all is no attempt at these records
                // ([`failed_attempt`]).
                reached_function: attempt.answered.is_some(),
            },
            &mut work.attempts,
            detail,
        );
        if let Some(attempts) = failed.discarded {
            // The batch is gone from the shard's reach, so a split it was being read in is
            // over too: the records behind it are read at the mapping's own `BatchSize`.
            work.bisected.remove(shard);
            // The records are gone from the function's reach, so the only thing a client can
            // still learn about them is what the mapping's own failure destination says.
            failure_destination::discard_batch(
                callback,
                &mut work.clients,
                &stores.policies,
                &failure_destination::FailedBatch {
                    uuid,
                    run,
                    shard,
                    // The batch the mapping gave up on is the one it HANDED OVER: a record
                    // its criteria rejected was never the function's to process, and one no
                    // batch of this pass took was never handed over at all.
                    records: given_up,
                    attempts,
                    answered: attempt.answered.as_ref(),
                },
            )
            .await;
        }
        failure = Some(failed.detail);
        undelivered = true;
    }
    // What the pass came to is published ONCE, for the mapping rather than per shard: the
    // counts are quantities of this mapping's own work, and a pass that read nothing out of
    // any shard says so with a 0-valued poll count, at most once a minute
    // ([`claims_idle_minute`]) — which is what makes an empty window mean "the poller looked
    // and found nothing" rather than "it is not publishing".
    if run.meters_event_counts {
        let at_secs = epoch_secs(SystemTime::now());
        if counts.polled > 0 {
            stores.metrics.publish_event_counts(uuid, &counts, at_secs);
            work.idle_minute = None;
        } else if claims_idle_minute(work, at_secs) {
            stores.metrics.publish_idle_poll(uuid, at_secs);
        }
    }
    match failure {
        Some(detail) => Outcome::Failed {
            detail,
            undelivered,
        },
        None if delivered => Outcome::Delivered,
        None => Outcome::Idle,
    }
}

/// A count of records as a metric carries it.
fn count_of(records: usize) -> u32 {
    u32::try_from(records).unwrap_or(u32::MAX)
}

/// `at` as epoch seconds, which is how a CloudWatch datapoint carries its instant.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(::std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Whether this poller may publish a 0-valued poll count at `at_secs` — true once per
/// minute, which is the resolution such a series is aggregated at — and claim that minute
/// when it may.
fn claims_idle_minute(work: &mut PollerWork, at_secs: f64) -> bool {
    const SECONDS_PER_MINUTE: f64 = 60.0;
    let minute = (at_secs / SECONDS_PER_MINUTE).trunc() as i64;
    if work.idle_minute == Some(minute) {
        return false;
    }
    work.idle_minute = Some(minute);
    true
}

/// How many records ONE batch of this mapping holds at most, as a count of records.
fn batch_records(run: &ShardSourcedMapping) -> usize {
    usize::try_from(run.batch_size).unwrap_or(ONE_BATCH_PER_SHARD).max(1)
}

/// Read what one pass takes off `shard`: the batches the mapping may have in flight for one
/// shard ([`in_flight_batches_per_shard`]), each read at the position the one before it
/// ended at, in the order the shard holds them.
///
/// `None` for a shard that has no position left to read from. Reading stops early at the
/// first batch the shard did not fill, because a shard that handed over less than a batch
/// has nothing more waiting — so a mapping with no backlog reads exactly the one batch a
/// serial pass would, and one draining a backlog reads as many as it may hand over at once.
/// It also stops once the pass holds [`MAX_RECORDS_PER_PASS`] records, so what one pass holds
/// in memory is bounded whatever `BatchSize` and `ParallelizationFactor` come to together.
async fn read_window(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    arrivals: &super::event_source_arrival::SourceArrivals,
    run: &ShardSourcedMapping,
    shard: &str,
    cursor: &ShardCursor,
) -> Result<Option<Vec<Vec<ShardRecord>>>, String> {
    let mut read: Vec<Vec<ShardRecord>> = Vec::new();
    let mut held = 0usize;
    let mut at = cursor.clone();
    for _ in 0..run.in_flight_batches.max(ONE_BATCH_PER_SHARD) {
        let Some(batch) = read_batch(callback, clients, arrivals, run, shard, &at).await? else {
            // "If the shard has been closed, the shard iterator can't return more data":
            // such a shard has no position left to read from.
            return Ok((!read.is_empty()).then_some(read));
        };
        let Some(last) = batch.last() else {
            break;
        };
        at = ShardCursor::AfterSequenceNumber(last.sequence_number.clone());
        let filled = batch.len() >= batch_records(run);
        held = held.saturating_add(batch.len());
        read.push(batch);
        if !filled || held >= MAX_RECORDS_PER_PASS {
            break;
        }
    }
    Ok(Some(read))
}

/// The partition key of each of `records`, in the same order — what decides which of a pass's
/// concurrent batches each record is handed over in ([`concurrent_batches`]).
fn grouping_keys(
    run: &ShardSourcedMapping,
    partition_key_attribute: Option<&str>,
    records: &[ShardRecord],
) -> Vec<String> {
    records
        .iter()
        .map(|record| match run.source {
            ShardSource::Kinesis => kinesis_stream::partition_key(&record.envelope),
            ShardSource::DynamoDbStream => {
                dynamodb_stream::partition_key(&record.envelope, partition_key_attribute)
            }
        })
        .collect()
}

/// Route `records` — what one pass read off ONE shard and its criteria admitted, in shard
/// order, each with its partition key in `keys` — to the batches the mapping has in flight
/// for that shard.
///
/// `ParallelizationFactor` is "the number of batches to process from each shard
/// concurrently", so `lanes` is how many batches may be in flight and `batch_size` is how
/// many records one of them holds. The routing is what keeps concurrency from reordering
/// anything a client may rely on: the records of ONE partition key all go to the SAME batch,
/// in shard order, so two records of one key are never in flight beside each other, while
/// records of different keys are. A key is given the next batch in turn as it first appears,
/// so the keys a pass read are spread over the batches rather than crowding one of them.
///
/// A record that would overflow its batch ends the routing: it and everything behind it are
/// left for a later pass, so a record is never handed over ahead of one the shard holds
/// before it. The answer holds no empty batch, and for a mapping with one batch in flight it
/// is exactly the one batch the pass read.
fn concurrent_batches(
    records: &[ShardRecord],
    keys: &[String],
    lanes: usize,
    batch_size: usize,
) -> Vec<Vec<ShardRecord>> {
    let lanes = lanes.max(ONE_BATCH_PER_SHARD);
    let mut batches: Vec<Vec<ShardRecord>> = vec![Vec::new(); lanes];
    let mut routed: BTreeMap<&str, usize> = BTreeMap::new();
    let mut taken = 0usize;
    for (record, key) in records.iter().zip(keys) {
        let next = taken % lanes;
        let lane = *routed.entry(key.as_str()).or_insert_with(|| {
            taken = taken.saturating_add(1);
            next
        });
        if batches[lane].len() >= batch_size.max(1) {
            break;
        }
        batches[lane].push(record.clone());
    }
    batches.retain(|batch| !batch.is_empty());
    batches
}

/// What the batches one pass had in flight for ONE shard came to, as the shard's own
/// position is decided from.
///
/// The records PROCESSED are every record any of those batches processed, since each of them
/// reports about the records it was handed. What the pass came to is the attempt on the batch
/// holding the shard's own next record — the first of what it read — because that is the
/// attempt the shard's position waits on: records behind a batch that did not process its
/// own are read again whatever the other batches came to.
fn merged_attempt(attempts: Vec<Attempt>) -> Attempt {
    let processed: Vec<String> = attempts
        .iter()
        .flat_map(|attempt| attempt.processed.iter().cloned())
        .collect();
    match attempts.into_iter().next() {
        Some(head) => Attempt {
            processed,
            answered: head.answered,
            failure: head.failure,
        },
        None => Attempt {
            processed,
            answered: None,
            failure: None,
        },
    }
}

/// Record one FAILED attempt at the batch `sequence_numbers` names on `shard`, and answer
/// what the attempt came to for the one log line a failing mapping writes.
///
/// What the attempt came to is reported as the mapping's `LastProcessingResult` either way.
/// The cursor stays where it was — so the same batch is read again — until the mapping has
/// spent on this batch every attempt it allows: "Discard records after the specified number
/// of retries" (`MaximumRetryAttempts`), at which point the shard is read on from
/// `discarded_through` — as far past the batch as the records ACCOUNTED FOR reach — so the
/// records behind the ones that were discarded are delivered, and one no batch of the pass
/// was handed is not read past. A mapping carrying the infinite default discards nothing and
/// keeps trying ([`attempts_allowed`]).
///
/// What the allowance counts is an attempt the batch was MADE WITH: the member is the maximum
/// number of times Lambda retries when the FUNCTION returns an error, and that does not apply
/// to a service error or a throttle where the batch never reached the function. So a pass that
/// could not invoke the function at all (`reached_function` false) spends none of the
/// allowance and discards nothing — the records stay in the shard for the pass that follows,
/// exactly as they do while a read is withheld — and only a batch the function answered for
/// counts against it.
fn failed_attempt(
    state: &::engine::ServiceState<ComAmazonawsLambdaAwsgirapiserviceState>,
    uuid: &str,
    run: &ShardSourcedMapping,
    shard: &str,
    batch: &GivenUpBatch<'_>,
    attempts: &mut BTreeMap<String, BatchAttempts>,
    detail: String,
) -> FailedAttempt {
    let sequence_numbers = batch.sequence_numbers;
    let Some(head) = sequence_numbers.first() else {
        return FailedAttempt {
            detail,
            discarded: None,
        };
    };
    if !batch.reached_function {
        checkpoint(
            state,
            uuid,
            run,
            shard,
            None,
            super::LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED,
        );
        return FailedAttempt {
            detail: format!(
                "{detail}; the batch never reached the function, so it is no attempt at those \
                 records and {shard} reads them again"
            ),
            discarded: None,
        };
    }
    let made = match attempts.get_mut(shard) {
        // The shard is still at the position this batch was read from, so this is one more
        // attempt at the SAME batch.
        Some(held) if &held.head == head => {
            held.made = held.made.saturating_add(1);
            held.made
        }
        // A batch read from a position the shard has since moved to is a different batch,
        // which gets the mapping's whole allowance of its own.
        _ => {
            attempts.insert(
                shard.to_string(),
                BatchAttempts {
                    head: head.clone(),
                    made: 1,
                },
            );
            1
        }
    };
    let discarded = run.attempts_allowed.is_some_and(|allowed| made >= allowed);
    if discarded {
        attempts.remove(shard);
    }
    checkpoint(
        state,
        uuid,
        run,
        shard,
        discarded
            .then(|| batch.discarded_through.map(str::to_string))
            .flatten(),
        super::LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED,
    );
    if discarded {
        return FailedAttempt {
            detail: format!(
                "{detail}, and the mapping allows {made} attempt(s) at a batch, so the {} \
                 record(s) of this one were discarded and {shard} reads on past them",
                sequence_numbers.len()
            ),
            discarded: Some(made),
        };
    }
    FailedAttempt {
        detail: format!("{detail}, so {shard} reads the same batch again"),
        discarded: None,
    }
}

/// The batch one failed attempt of a pass was at, as [`failed_attempt`] accounts for it: the
/// records it held, and where the shard reads on from once the mapping gives them up.
struct GivenUpBatch<'pass> {
    /// The records of the batch the attempt was at, by sequence number, in the order the
    /// shard holds them.
    sequence_numbers: &'pass [String],
    /// Where the shard reads on from once this batch is discarded: as far past it as the
    /// records the pass accounted for reach, and `None` when that is nowhere, which leaves
    /// the position where it was.
    discarded_through: Option<&'pass str>,
    /// Whether the batch REACHED the function — whether an invocation was made with it at
    /// all — which is what decides whether this failure is one of the attempts the mapping
    /// allows the batch ([`failed_attempt`]).
    reached_function: bool,
}

/// What one failed attempt at a batch came to: the sentence it is logged with, and — when
/// this attempt was the last one the mapping allows that batch — how many attempts the
/// discarded batch had in all.
///
/// The count travels because the record of a discarded batch reports it
/// ([`failure_destination`]); a batch that is to be attempted again carries none, and is
/// recorded nowhere.
struct FailedAttempt {
    /// Why the attempt did not process the batch, and what became of the batch.
    detail: String,
    /// The attempts the batch had, for the batch that was DISCARDED; `None` for one the
    /// mapping will attempt again.
    discarded: Option<u32>,
}

/// The event ONE batch is handed over as: "Lambda reads items from the event source and
/// invokes the function", and what it invokes the function with carries one envelope per
/// record HANDED OVER, under `Records`, in the order the shard holds them — so a record the
/// mapping's criteria rejected is in no invocation at all ([`admitted_of`]).
///
/// Each envelope is the one the record's own source built for it ([`ShardRecord::envelope`]),
/// so this composition is the one part of the hand-over that is the same for every source.
fn batch_event(records: &[ShardRecord]) -> Value {
    json!({
        "Records": records
            .iter()
            .map(|record| record.envelope.clone())
            .collect::<Vec<Value>>(),
    })
}

/// Make the FINAL invocation the tumbling window of `shard` owes, for a window that is over.
///
/// A window that has ended is finalised before anything more is read off that shard: the
/// function is invoked once more with NO records, the state the window reached and
/// `isFinalInvokeForWindow` true ([`tumbling_window`]), which is where a handler aggregating
/// over the window writes its result. Only then does the shard read on, so the records that
/// follow aggregate into the window the clock is in rather than into the one that is over.
///
/// An invocation the function did not answer leaves the window where it is and reports the
/// pass as failed, so nothing is read off the shard on this pass and the final invocation is
/// attempted again — until the mapping has spent on it every attempt it allows, which is the
/// same allowance a batch its function keeps failing gets ([`attempts_allowed`]). A mapping
/// that configures no window, and a shard whose window is still open, owe nothing here.
async fn close_due_window(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    work: &mut PollerWork,
    run: &ShardSourcedMapping,
    shard: &str,
    now: SystemTime,
) -> Result<(), String> {
    if run.tumbling_window.is_none() {
        return Ok(());
    }
    let Some(window) = tumbling_window::due_to_close(&work.windows, shard, now) else {
        return Ok(());
    };
    // "Lambda ... a final invocation": the records of the window have already been handed
    // over, so what this invocation carries is the window itself and the state it reached.
    let mut payload = batch_event(&[]);
    tumbling_window::attach(&mut payload, &run.stream_arn, shard, &window, true);
    let client = work.clients.lambda(callback, &run.region).clone();
    let detail = match invoke_function(&client, run, &payload).await {
        Ok(answered) => match answered.function_error {
            None => {
                tumbling_window::closed(&mut work.windows, shard);
                return Ok(());
            }
            // "If present, indicates that an error occurred during function execution"
            // (`InvocationResponse$FunctionError`): the window was not finalised.
            Some(failure) => format!(
                "{} answered a {failure} error for the final invocation of {shard}'s tumbling \
                 window",
                run.function_arn
            ),
        },
        Err(detail) => detail,
    };
    if tumbling_window::final_attempt_failed(&mut work.windows, shard, run.attempts_allowed) {
        return Err(format!(
            "{detail}, and the mapping allows no further attempt at it, so that window is \
             given up on and {shard} aggregates into the window the clock is in"
        ));
    }
    Err(format!(
        "{detail}, so the final invocation of that window is attempted again"
    ))
}

/// Every open and closed shard of the mapped stream, in the order the stream reports them,
/// and how the records of one shard are grouped into the concurrent batches of a pass —
/// through the listing the source's own data plane publishes.
async fn shard_ids(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    run: &ShardSourcedMapping,
) -> Result<StreamShards, String> {
    match run.source {
        ShardSource::Kinesis => kinesis_stream::shard_ids(callback, clients, run).await,
        ShardSource::DynamoDbStream => dynamodb_stream::shard_ids(callback, clients, run).await,
    }
}

/// What a stream's own listing says about reading it: its shards, and the attribute a record
/// of it carries its partition key under for the sources that report one apart from the
/// record.
pub(super) struct StreamShards {
    /// Every open and closed shard, in the order the stream reports them.
    pub(super) shards: Vec<String>,
    /// The attribute of a record's key that is the PARTITION key of the item it changed, for
    /// a source whose records carry the whole key: a DynamoDB table's stream reports the key
    /// schema of the table it is on, whose `HASH` element names that attribute. `None` for a
    /// source whose record carries its partition key directly (a Kinesis record does), and
    /// for a listing that reported no key schema to read it from.
    pub(super) partition_key_attribute: Option<String>,
}


/// The records one batch holds: what `shard` has at `cursor`, up to the mapping's batch
/// size, each already rendered as the envelope its own source hands a record over as and
/// stamped with the instant it was WRITTEN ([`stamped_with_the_write`]).
///
/// `None` for a shard that has no position left to read from.
async fn read_batch(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    arrivals: &super::event_source_arrival::SourceArrivals,
    run: &ShardSourcedMapping,
    shard: &str,
    cursor: &ShardCursor,
) -> Result<Option<Vec<ShardRecord>>, String> {
    let read = match run.source {
        ShardSource::Kinesis => {
            kinesis_stream::read_batch(callback, clients, run, shard, cursor).await?
        }
        ShardSource::DynamoDbStream => {
            dynamodb_stream::read_batch(callback, clients, run, shard, cursor).await?
        }
    };
    Ok(read.map(|batch| stamped_with_the_write(arrivals, run, batch)))
}

/// `batch` with every record carrying the instant it was WRITTEN to the shard, which is the
/// one instant a record's age is measured from ([`record_age::age_of`]) — both the
/// `IteratorAge` a pass meters and the age `MaximumRecordAgeInSeconds` discards a record for.
///
/// Which instant that is belongs to the SOURCE, and the stamp the read carried is it for one
/// of the two:
///
/// * an Amazon Kinesis record's `ApproximateArrivalTimestamp` is "the approximate time that
///   the record was inserted into the stream" at the resolution that service stamped it, so
///   the batch is taken as read;
/// * a DynamoDB stream record's `ApproximateCreationDateTime` is "rounded down to the closest
///   second", so it names a moment up to a second BEFORE the write and an age measured from it
///   is that much too long. The instant of the write is read out of the store of the service
///   that holds the record ([`super::event_source_arrival`]), in one lock for the batch, and
///   the stamp the read carried stands for a record that service no longer retains.
///
/// Only what the age is measured FROM moves: the stamp each record was read with is left as
/// the source published it, so what a report of the record carries — its envelope, and the
/// arrival members of the record written to the mapping's failure destination — is the member
/// that source answers, at the resolution it answers it at.
fn stamped_with_the_write(
    arrivals: &super::event_source_arrival::SourceArrivals,
    run: &ShardSourcedMapping,
    batch: Vec<ShardRecord>,
) -> Vec<ShardRecord> {
    match run.source {
        ShardSource::Kinesis => batch,
        ShardSource::DynamoDbStream => {
            let sequence_numbers: Vec<String> = batch
                .iter()
                .map(|record| record.sequence_number.clone())
                .collect();
            let written = arrivals.table_stream_writes(&run.stream_arn, &sequence_numbers);
            batch
                .into_iter()
                .zip(written)
                .map(|(mut record, written)| {
                    if let Some(written) = written {
                        record.written_at = Some(::aws_smithy_types::DateTime::from(written));
                    }
                    record
                })
                .collect()
        }
    }
}

/// Record what one processing attempt came to on the mapping itself, and — for an attempt
/// that delivered its batch — move the shard's cursor past that batch.
///
/// The record is re-read under the lock and only written when it still names the stream and
/// the function this batch was read for, so a mapping re-targeted while the batch was in
/// flight is not checkpointed under a configuration it no longer carries.
fn checkpoint(
    state: &::engine::ServiceState<ComAmazonawsLambdaAwsgirapiserviceState>,
    uuid: &str,
    run: &ShardSourcedMapping,
    shard: &str,
    delivered_through: Option<String>,
    result: &str,
) {
    let mut store = state.lock();
    let Some(mapping) = store.event_source_mappings.get_mut(uuid) else {
        return;
    };
    if mapping.event_source_arn.as_deref() != Some(run.stream_arn.as_str())
        || mapping.function_arn != run.function_arn
    {
        return;
    }
    if let Some(sequence_number) = delivered_through {
        mapping
            .shard_cursors
            .insert(shard.to_string(), ShardCursor::AfterSequenceNumber(sequence_number));
    }
    mapping.last_processing_result = Some(result.to_string());
}

/// How long the newest record of a batch had been in the shard when the batch was read,
/// in milliseconds: never negative, and 0 for a record this emulator holds no write
/// instant for.
fn iterator_age_millis(
    written_at: Option<&::aws_smithy_types::DateTime>,
    read_at: SystemTime,
) -> f64 {
    // The age itself is the ONE reading of how old a record is ([`record_age::age_of`]) —
    // the same one the mapping's own record age is decided by — so the figure a client
    // reads and the decision the poller takes cannot drift apart.
    record_age::age_of(written_at, read_at).map_or(0.0, |age| age.as_secs_f64() * 1_000.0)
}

/// Hand one batch to the function and read what became of the records it held: the ones the
/// attempt PROCESSED, or why none of them was.
///
/// Every record of a batch an invocation accepted was processed unless the mapping asks for
/// its function's report of which of them failed to be read, which is what
/// `FunctionResponseTypes` says (see [`super::batch_item_failures`]).
async fn hand_over_batch(
    client: &::aws_sdk_lambda::Client,
    run: &ShardSourcedMapping,
    payload: &Value,
    sequence_numbers: &[String],
) -> Attempt {
    let answered = match invoke_function(client, run, payload).await {
        Ok(answered) => answered,
        // The invocation was not made at all, so there is nothing it answered.
        Err(detail) => {
            return Attempt {
                processed: Vec::new(),
                answered: None,
                failure: Some(detail),
            }
        }
    };
    // "If present, indicates that an error occurred during function execution"
    // (`InvocationResponse$FunctionError`): the batch was not processed, so it is not
    // checkpointed past.
    if let Some(failure) = answered.function_error.as_deref() {
        let detail = format!("{} answered a {failure} error for the batch", run.function_arn);
        return Attempt {
            processed: Vec::new(),
            answered: Some(answered),
            failure: Some(detail),
        };
    }
    if !run.reports_batch_item_failures {
        return Attempt {
            processed: sequence_numbers.to_vec(),
            answered: Some(answered),
            failure: None,
        };
    }
    match super::read_batch_item_failures(answered.payload.as_deref(), sequence_numbers) {
        super::BatchReport::WholeBatchFailed(detail) => Attempt {
            processed: Vec::new(),
            answered: Some(answered),
            failure: Some(format!(
                "{} reported no record of the batch as processed: {detail}",
                run.function_arn
            )),
        },
        report => Attempt {
            processed: super::processed_records(&report, sequence_numbers).unwrap_or_default(),
            answered: Some(answered),
            failure: None,
        },
    }
}

/// The sequence number a shard reads on from after an attempt that processed `processed` of
/// the batch `sequence_numbers`, or `None` when the attempt did not process the batch's own
/// first record and so moved the shard's position not at all.
///
/// A shard has ONE position, so the records it can be read on from past are the run at the
/// HEAD of the batch that were processed: the first record the attempt did not process is
/// the one the next read must start at.
fn checkpointed_through(sequence_numbers: &[String], processed: &[String]) -> Option<String> {
    let consumed: ::std::collections::BTreeSet<&str> =
        processed.iter().map(String::as_str).collect();
    sequence_numbers
        .iter()
        .take_while(|sequence_number| consumed.contains(sequence_number.as_str()))
        .last()
        .cloned()
}

/// Hand one batch to the function, synchronously, as a stream event source's invocation is,
/// and answer with everything the invocation reported about it.
async fn invoke_function(
    client: &::aws_sdk_lambda::Client,
    run: &ShardSourcedMapping,
    payload: &Value,
) -> Result<Answered, String> {
    let answer = client
        .invoke()
        .function_name(&run.function_arn)
        .payload(::aws_sdk_lambda::primitives::Blob::new(
            payload.to_string().into_bytes(),
        ))
        .send()
        .await
        .map_err(|error| {
            format!(
                "{} could not be invoked ({})",
                run.function_arn,
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
    Ok(Answered {
        // "The HTTP status code is in the 200 range for a successful request"
        // (`InvocationResponse$StatusCode`).
        status_code: Some(answer.status_code),
        // "The version of the function that executed"
        // (`InvocationResponse$ExecutedVersion`).
        executed_version: answer.executed_version.clone(),
        function_error: answer.function_error.clone(),
        request_id: ::aws_types::request_id::RequestId::request_id(&answer).map(str::to_string),
        // "The response from the function, or an error object"
        // (`InvocationResponse$Payload`): the document a mapping that reads a batch item
        // failure report reads it out of.
        payload: answer.payload.map(::aws_smithy_types::Blob::into_inner),
    })
}

/// The clients this data plane reaches the other services of this emulator through, built
/// on first use and then reused.
///
/// Each slot remembers the Region its client was built for, because a resource is reached
/// in the Region ITS OWN ARN names.
///
/// Shared with [`super::sqs_poller`], the reader of the other kind of event source, so both
/// pollers reach this emulator the one way.
#[derive(Default)]
pub(crate) struct Clients {
    kinesis: Option<(String, ::aws_sdk_kinesis::Client)>,
    dynamodbstreams: Option<(String, ::aws_sdk_dynamodbstreams::Client)>,
    lambda: Option<(String, ::aws_sdk_lambda::Client)>,
    /// The destination services the record of a discarded batch is written through
    /// ([`failure_destination`]).
    sqs: Option<(String, ::aws_sdk_sqs::Client)>,
    /// See [`Self::sqs`].
    sns: Option<(String, ::aws_sdk_sns::Client)>,
    /// See [`Self::sqs`]. Built by the workspace's one construction of the Amazon S3 client
    /// an emulated service writes an object through rather than by the macro below, since an
    /// object store takes a configuration of its own: it is addressed path-style, and the
    /// object it stores carries the properties the write set and no others.
    s3: Option<(String, ::aws_sdk_s3::Client)>,
}

/// Build each client the same way: over the in-process callback into this same emulator,
/// against the called service's endpoint in `region`.
///
/// The endpoint url names the called service's public host so the request is signed and
/// routed as that service's, but the TRANSPORT is the emulator's own in-process connector
/// ([`smithy_server_runtime::EmbeddedEmulator::http_client`]): no socket is opened and no
/// name is resolved, so a stream or function a mapping named is reached only if this
/// emulator serves it itself.
///
/// The transport is in-process, so each client disables what a network client needs and
/// this one cannot have: retries, timeouts, stalled-stream protection and identity caching
/// each need a sleep implementation the callback transport is not given.
macro_rules! callback_client {
    ($field:ident, $sdk:ident, $endpoint:literal) => {
        pub(crate) fn $field(
            &mut self,
            callback: &smithy_server_runtime::EmbeddedEmulator,
            region: &str,
        ) -> &::$sdk::Client {
            if self
                .$field
                .as_ref()
                .is_none_or(|(held, _)| held != region)
            {
                let config = ::$sdk::Config::builder()
                    .behavior_version(::$sdk::config::BehaviorVersion::latest())
                    .http_client(callback.http_client())
                    .credentials_provider(callback.aws_credentials_provider())
                    .region(::$sdk::config::Region::new(region.to_string()))
                    .endpoint_url(format!("https://{}.{region}.amazonaws.com", $endpoint))
                    .retry_config(::$sdk::config::retry::RetryConfig::disabled())
                    .timeout_config(::$sdk::config::timeout::TimeoutConfig::disabled())
                    .identity_cache(::$sdk::config::IdentityCache::no_cache())
                    .stalled_stream_protection(
                        ::$sdk::config::StalledStreamProtectionConfig::disabled(),
                    )
                    .build();
                self.$field = Some((region.to_string(), ::$sdk::Client::from_conf(config)));
            }
            &self
                .$field
                .as_ref()
                .expect("the client for this region was just built")
                .1
        }
    };
}

impl Clients {
    /// The Amazon S3 client the record of a discarded batch is written as an object through,
    /// remembering the Region it was built for exactly as every slot above does.
    pub(crate) fn s3(
        &mut self,
        callback: &smithy_server_runtime::EmbeddedEmulator,
        region: &str,
    ) -> &::aws_sdk_s3::Client {
        if self.s3.as_ref().is_none_or(|(held, _)| held != region) {
            self.s3 = Some((
                region.to_string(),
                ::service_common::s3_client::s3_client(callback, region),
            ));
        }
        &self
            .s3
            .as_ref()
            .expect("the client for this region was just built")
            .1
    }

    callback_client!(kinesis, aws_sdk_kinesis, "kinesis");
    callback_client!(dynamodbstreams, aws_sdk_dynamodbstreams, "streams.dynamodb");
    callback_client!(lambda, aws_sdk_lambda, "lambda");
    callback_client!(sqs, aws_sdk_sqs, "sqs");
    callback_client!(sns, aws_sdk_sns, "sns");
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{
        batch_event, checkpointed_through, iterator_age_millis, stamped_with_the_write,
        ShardCursor, ShardRecord, ShardSource, ShardSourcedMapping,
    };

    /// A record carrying `sequence_number`, as the poller routes one.
    fn record(sequence_number: &str) -> ShardRecord {
        ShardRecord {
            sequence_number: sequence_number.to_string(),
            arrived_at: None,
            written_at: None,
            envelope: ::serde_json::json!({ "eventID": sequence_number }),
        }
    }

    /// A pass over `stream_arn` of `source`, with everything a pass carries beside those two
    /// left at what a mapping configuring nothing has: the one thing the stamping reads off a
    /// pass is WHICH source it is over.
    fn run_over(source: ShardSource, stream_arn: &str) -> ShardSourcedMapping {
        ShardSourcedMapping {
            source,
            stream_arn: stream_arn.to_string(),
            region: "us-east-1".to_string(),
            function_arn: "arn:aws:lambda:us-east-1:000000000000:function:metered".to_string(),
            function_name: "metered".to_string(),
            invoke_identity_arn: None,
            batch_size: 1,
            batching_window: Duration::ZERO,
            in_flight_batches: 1,
            filters: Vec::new(),
            reports_batch_item_failures: false,
            attempts_allowed: None,
            bisects_on_function_error: false,
            on_failure_destination: None,
            maximum_record_age: None,
            tumbling_window: None,
            meters_event_counts: false,
            cursors: ::std::collections::BTreeMap::new(),
            initial_cursor: ShardCursor::TrimHorizon,
        }
    }

    /// RULE: `ParallelizationFactor` is how many batches of one shard a mapping has in
    /// flight, held to what the function's own reservation and this process's run capacity
    /// admit; a mapping configuring none has one.
    ///
    /// The rows differ along what the rule generalises over: the value configured (none, each
    /// end of the published range, one inside it, and values outside it either way), and what
    /// the function reserves.
    #[test]
    fn the_batches_one_shard_yields_are_what_the_mapping_configures() {
        let host = i64::from(crate::state::execution::MAX_CONCURRENT_RUNS);
        // (case, `ParallelizationFactor`, `ReservedConcurrentExecutions`, the batches)
        let rows: Vec<(&str, Option<i32>, Option<i32>, usize)> = vec![
            ("none-configured", None, None, 1),
            ("the-published-minimum", Some(1), None, 1),
            ("inside-the-published-range", Some(4), None, 4),
            (
                "the-published-maximum",
                Some(10),
                None,
                usize::try_from(host.min(10)).unwrap_or(1),
            ),
            ("below-the-published-range", Some(0), None, 1),
            (
                "above-the-published-range",
                Some(1_000),
                None,
                usize::try_from(host.min(10)).unwrap_or(1),
            ),
            ("a-reservation-below-it", Some(6), Some(2), 2),
            ("a-reservation-above-it", Some(2), Some(50), 2),
            ("a-reservation-of-none", Some(3), Some(0), 1),
        ];
        for (case, configured, reserved, expected) in rows {
            let actual = super::in_flight_batches_per_shard(configured, reserved);
            assert_eq!(
                actual, expected,
                "case {case:?}: a mapping configuring {configured:?} with a reservation of \
                 {reserved:?} must have {expected} batch(es) of one shard in flight, got \
                 {actual}"
            );
        }
    }

    /// RULE: a mapping that aggregates over a tumbling window reads one shard SERIALLY —
    /// one batch of it in flight — whatever `ParallelizationFactor` it also carries, because
    /// the state of that window is the shard's and is handed from one invocation of it to the
    /// next; a mapping with no window has what that factor asks for.
    ///
    /// The rows differ along what the rule generalises over: the window configured (none,
    /// durations either side of the range it admits) crossed with the factor configured.
    #[test]
    fn a_mapping_aggregating_over_a_window_reads_its_shard_serially() {
        // (case, the tumbling window, `ParallelizationFactor`, the batches in flight)
        let rows: Vec<(&str, Option<Duration>, Option<i32>, usize)> = vec![
            ("no-window-no-factor", None, None, 1),
            ("no-window-with-a-factor", None, Some(4), 4),
            ("a-one-second-window", Some(Duration::from_secs(1)), None, 1),
            (
                "a-window-with-a-factor-beside-it",
                Some(Duration::from_secs(30)),
                Some(4),
                1,
            ),
            (
                "the-largest-window-with-the-largest-factor",
                Some(Duration::from_secs(900)),
                Some(10),
                1,
            ),
        ];
        for (case, window, configured, expected) in rows {
            let actual = super::in_flight_batches_of(window, configured, None);
            assert_eq!(
                actual, expected,
                "case {case:?}: a mapping aggregating over {window:?} and configuring \
                 {configured:?} must have {expected} batch(es) of one shard in flight, got \
                 {actual}"
            );
        }
    }

    /// RULE: the records one pass read off a shard are routed to the batches the mapping has
    /// in flight — the records of ONE partition key to the SAME batch, in shard order, each
    /// batch holding at most the mapping's `BatchSize` — and a record that would overflow its
    /// batch, and everything behind it, is left for a later pass.
    ///
    /// The rows differ along what the rule generalises over: how many batches may be in
    /// flight, how large one is, how many keys the records carry and how they are spread over
    /// them.
    #[test]
    fn the_records_of_one_key_are_routed_to_one_batch() {
        // One row: its case, the (sequence number, partition key) of each record read, how
        // many batches may be in flight, how large one is, and the batches the records must be
        // routed to, each by the sequence numbers it holds.
        type Row<'row> = (
            &'row str,
            Vec<(&'row str, &'row str)>,
            usize,
            usize,
            Vec<Vec<&'row str>>,
        );
        let rows: Vec<Row<'_>> = vec![
            (
                "one batch in flight takes the whole read",
                vec![("1", "a"), ("2", "b"), ("3", "c")],
                1,
                3,
                vec![vec!["1", "2", "3"]],
            ),
            (
                "two batches over records of two keys",
                vec![("1", "a"), ("2", "b"), ("3", "a"), ("4", "b")],
                2,
                2,
                vec![vec!["1", "3"], vec!["2", "4"]],
            ),
            (
                "the keys are spread over the batches in turn",
                vec![("1", "a"), ("2", "b"), ("3", "c"), ("4", "d")],
                2,
                1,
                vec![vec!["1"], vec!["2"]],
            ),
            (
                "records of one key alone fill one batch",
                vec![("1", "a"), ("2", "a"), ("3", "a")],
                3,
                1,
                vec![vec!["1"]],
            ),
            (
                "more batches in flight than keys read",
                vec![("1", "a"), ("2", "b")],
                4,
                10,
                vec![vec!["1"], vec!["2"]],
            ),
            (
                "a batch that fills ends the routing",
                vec![("1", "a"), ("2", "a"), ("3", "b")],
                2,
                1,
                vec![vec!["1"]],
            ),
        ];
        for (case, read, lanes, batch_size, expected) in rows {
            let records: Vec<ShardRecord> = read
                .iter()
                .map(|(sequence_number, _)| record(sequence_number))
                .collect();
            let keys: Vec<String> = read.iter().map(|(_, key)| (*key).to_string()).collect();
            let batches = super::concurrent_batches(&records, &keys, lanes, batch_size);
            let routed: Vec<Vec<String>> = batches
                .iter()
                .map(|batch| {
                    batch
                        .iter()
                        .map(|record| record.sequence_number.clone())
                        .collect()
                })
                .collect();
            assert_eq!(
                routed, expected,
                "case {case:?}: {read:?} read into {lanes} batch(es) of {batch_size} must be \
                 routed to {expected:?}, got {routed:?}"
            );
        }
    }

    #[test]
    fn a_shard_reads_on_from_the_processed_run_at_the_head_of_the_batch() {
        // A shard has ONE position, so what it reads on from is the run of records at the
        // head of the batch the attempt processed. The rows differ along where in the batch
        // the first unprocessed record is: nowhere (every record processed), the head, the
        // middle, the tail, and a batch none of which was processed.
        let batch: Vec<String> = ["49-1", "49-2", "49-3"]
            .iter()
            .map(|sequence_number| (*sequence_number).to_string())
            .collect();
        let rows: [(&str, Vec<&str>, Option<&str>); 5] = [
            (
                "every record processed",
                vec!["49-1", "49-2", "49-3"],
                Some("49-3"),
            ),
            ("the first record failed", vec!["49-2", "49-3"], None),
            ("the middle record failed", vec!["49-1", "49-3"], Some("49-1")),
            ("the last record failed", vec!["49-1", "49-2"], Some("49-2")),
            ("no record processed", Vec::new(), None),
        ];
        for (case, processed, expected) in rows {
            let processed: Vec<String> = processed
                .iter()
                .map(|sequence_number| (*sequence_number).to_string())
                .collect();
            let through = checkpointed_through(&batch, &processed);
            assert_eq!(
                through.as_deref(),
                expected,
                "case {case:?}: a shard whose attempt processed {processed:?} of {batch:?} \
                 must read on from {expected:?}, got {through:?}"
            );
        }
    }

    /// RULE: a record a pass read carries the instant it was WRITTEN to the shard, which is
    /// the one instant its age is measured from — read out of the store of the service that
    /// holds the record where the stamp that service PUBLISHES for it is rounded down to a
    /// coarser resolution than the write, and taken as read where that stamp is the write
    /// instant itself.
    ///
    /// The rows differ along what the rule generalises over: the sub-second phase the write
    /// fell on (the second's boundary, a millisecond in, the middle, a recorded phase, and
    /// the last nanosecond before the next second), which is exactly what rounding a stamp
    /// down to the second discards. Each row asserts BOTH sources, so the choice is the
    /// source's own rather than one arm's.
    #[test]
    fn a_record_a_pass_read_carries_the_instant_it_was_written() {
        use ::service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::ops::types::{
            AttributeValue, KeySchemaElement,
        };
        use ::service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::state as streams;

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // (case, the sub-second phase the write fell on)
        let rows: Vec<(&str, Duration)> = vec![
            ("on-the-second", Duration::ZERO),
            ("one-millisecond-in", Duration::from_millis(1)),
            ("mid-second", Duration::from_millis(500)),
            ("403-milliseconds-in", Duration::from_millis(403)),
            ("the-last-nanosecond", Duration::new(0, 999_999_999)),
        ];
        for (case, phase) in rows {
            let engine = ::engine::InMemoryEngine::new();
            let store = engine
                .state::<streams::ComAmazonawsDynamodbstreamsDynamodbstreams20120810State>();
            let stream_arn = store
                .lock()
                .register_stream(
                    &streams::StreamRegistration {
                        table_name: "metered".to_string(),
                        stream_view_type: streams::StreamViewType::NewImage,
                        key_schema: vec![KeySchemaElement {
                            attribute_name: "Id".to_string(),
                            key_type: "HASH".to_string(),
                        }],
                        region: "us-east-1".to_string(),
                    },
                    base,
                )
                .expect("the registry takes the registration");
            let keys: streams::AttributeMap = [(
                "Id".to_string(),
                AttributeValue::S("metered".to_string()),
            )]
            .into_iter()
            .collect();
            let written = base + Duration::from_secs(10) + phase;
            let sequence_number = store
                .lock()
                .append_record(
                    &stream_arn,
                    &streams::RecordAppend {
                        event_name: streams::OperationType::Insert,
                        keys: keys.clone(),
                        new_image: Some(keys),
                        old_image: None,
                    },
                    written,
                )
                .expect("the registry takes the record");
            // The stamp a READ of this source carries: the write instant "rounded down to
            // the closest second", which is the member `GetRecords` reports.
            let stamp = ::aws_smithy_types::DateTime::from(streams::floor_to_second(written));
            let read = vec![ShardRecord {
                sequence_number,
                arrived_at: Some(stamp),
                written_at: Some(stamp),
                envelope: ::serde_json::json!({ "eventID": "metered" }),
            }];
            let arrivals = super::super::event_source_arrival::SourceArrivals::of(&engine);

            let stamped = stamped_with_the_write(
                &arrivals,
                &run_over(ShardSource::DynamoDbStream, &stream_arn),
                read.clone(),
            );
            let measured = stamped.first().and_then(|record| record.written_at);
            assert_eq!(
                measured,
                Some(::aws_smithy_types::DateTime::from(written)),
                "case {case:?}: expected a table-stream record to carry the write {written:?}, \
                 got {measured:?}"
            );
            // What the record REPORTS is left as its source published it, so nothing a client
            // reads about the record moves with the instant its age is measured from.
            let reported = stamped.first().and_then(|record| record.arrived_at);
            assert_eq!(
                reported,
                Some(stamp),
                "case {case:?}: expected the published stamp {stamp:?} to be left as read, got \
                 {reported:?}"
            );
            // Measured from the write, a batch read two seconds later waited exactly those two
            // seconds — whatever phase the write fell on.
            let aged = iterator_age_millis(measured.as_ref(), written + Duration::from_secs(2));
            assert!(
                (aged - 2_000.0).abs() < 1.0,
                "case {case:?}: expected an age of 2000 ms, got {aged:?} ms"
            );

            let stamped = stamped_with_the_write(
                &arrivals,
                &run_over(ShardSource::Kinesis, &stream_arn),
                read,
            );
            let measured = stamped.first().and_then(|record| record.written_at);
            assert_eq!(
                measured,
                Some(stamp),
                "case {case:?}: expected a data-stream record to keep the stamp the read \
                 carried {stamp:?}, got {measured:?}"
            );
        }
    }

    /// RULE: the age one batch meters is how long the NEWEST record of that batch had been
    /// in the shard when the batch was read — never negative, and 0 for a record with no
    /// write instant to measure from.
    ///
    /// The rows differ along what the rule generalises over: how long ago the record was
    /// written (now, a second ago, a minute and a half ago — above one CloudWatch period),
    /// a record whose write is ahead of the read, and one carrying no write instant at
    /// all.
    #[test]
    fn the_age_a_batch_meters_is_the_wait_of_its_newest_record() {
        let read_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // (case, when the newest record was written, the age in milliseconds)
        let rows: Vec<(&str, Option<SystemTime>, f64)> = vec![
            ("read-as-it-arrives", Some(read_at), 0.0),
            (
                "a-second-old",
                Some(read_at - Duration::from_secs(1)),
                1_000.0,
            ),
            (
                "held-past-one-period",
                Some(read_at - Duration::from_secs(90)),
                90_000.0,
            ),
            (
                "written-after-the-read",
                Some(read_at + Duration::from_secs(5)),
                0.0,
            ),
            ("no-write-instant", None, 0.0),
        ];
        for (case, written_at, expected) in rows {
            let written_at = written_at.map(::aws_smithy_types::DateTime::from);
            let actual = iterator_age_millis(written_at.as_ref(), read_at);
            assert!(
                (actual - expected).abs() < 1.0,
                "case {case:?}: expected an age of {expected} ms, got {actual:?} ms"
            );
        }
    }

    /// RULE: the event a batch is handed over as carries ONE envelope per record read,
    /// under `Records`, in the order the shard holds them — each the envelope the record's
    /// own source built for it.
    ///
    /// The rows differ along what the rule generalises over: how many records the batch
    /// holds (one, several, and — for completeness of the composition itself — none).
    #[test]
    fn a_batch_is_handed_over_as_one_envelope_per_record_in_shard_order() {
        // (case, the envelopes of the batch's records, in the order they were read)
        let rows: Vec<(&str, Vec<::serde_json::Value>)> = vec![
            ("no-record", Vec::new()),
            (
                "one-record",
                vec![::serde_json::json!({ "eventID": "only" })],
            ),
            (
                "three-records",
                vec![
                    ::serde_json::json!({ "eventID": "first" }),
                    ::serde_json::json!({ "eventID": "second" }),
                    ::serde_json::json!({ "eventID": "third" }),
                ],
            ),
        ];
        for (case, envelopes) in rows {
            let records: Vec<ShardRecord> = envelopes
                .iter()
                .enumerate()
                .map(|(index, envelope)| ShardRecord {
                    sequence_number: format!("1000000000000000000{index}"),
                    arrived_at: None,
                    written_at: None,
                    envelope: envelope.clone(),
                })
                .collect();
            let event = batch_event(&records);
            assert_eq!(
                event["Records"],
                ::serde_json::json!(envelopes),
                "case {case:?}: expected one envelope per record in shard order, got {event}"
            );
        }
    }
}
