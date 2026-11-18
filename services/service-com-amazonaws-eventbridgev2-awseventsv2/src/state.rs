// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-eventbridgev2-awseventsv2 service.
//!
//! This holds the event-bus registry every bus-addressed operation reads and
//! writes: `CreateEventBus` mints a bus here, `DescribeEventBus` /
//! `ListEventBuses` project it, `PutEvents` appends to its event log and
//! `DeleteEventBus` retires it. Nothing in a handler is computed from the
//! request alone — every field a response carries is read back out of here.
//!
//! The resources that hang off a bus live in their own modules, re-exported
//! here so every handler reaches one store: [`event_sources`] holds the
//! EventSource registry and [`subscribers`] the subscriber registry, whose
//! deliveries are made by [`subscriber_delivery`], and [`resource_policies`]
//! holds the named policies attached to a bus. [`publishing`] is what the two
//! publish operations share about an entry they are about to store.
//! [`pagination`] is the one reading of the page size and the page token every
//! List operation shares, and the `tags` module resolves one taggable ARN
//! against all three registries, because tagging spans them.

mod event_sources;
mod pagination;
mod publishing;
mod resource_policies;
pub mod subscriber_delivery;
mod subscribers;
mod tags;

pub use event_sources::*;
pub use pagination::*;
pub use publishing::*;
pub use resource_policies::*;
pub use subscribers::*;

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime};

use crate::ops::types::{
    EncryptionConfiguration, ResourceNotFoundException, StorageConfiguration,
    StorageConfigurationOutput,
};

/// Region used when the request carries no SigV4 credential scope to read one
/// from (an unsigned in-process call), matching the emulator's default region.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The `arnNamespace` of this service, per its `aws.api#service` trait — the
/// `events` segment of every `EventBusArn`.
pub const ARN_SERVICE: &str = "events";

/// The resource-type segment of an `EventBusArn`, from the modelled pattern
/// `^arn:aws…:events:…:…:event-busv2\/<name>\/[a-z0-9]{25}$`.
pub const ARN_RESOURCE_TYPE: &str = "event-busv2";

/// Width of the id segment of an `EventBusArn`: the modelled pattern pins it at
/// exactly `[a-z0-9]{25}`.
pub const BUS_ID_LEN: usize = 25;

/// How long after the call that starts it a bus lifecycle transition becomes
/// due.
///
/// `CreateEventBus` documents "Provisioning is asynchronous: the bus is returned
/// in the CREATING state and transitions to ACTIVE when ready (see the
/// EventBusActive waiter)", and `DeleteEventBus` documents "Deletion is
/// asynchronous: the bus moves to DELETING and disappears when complete (see the
/// EventBusDeleted waiter)". Neither span is a documented duration, so this is a
/// chosen one: short enough for a waiter to finish promptly, long enough that a
/// caller acting straight after the call still observes the transient state and
/// the state errors that go with it. No handler ever sleeps for it — the due
/// instant is stored and the transition is materialised by whichever later call
/// first observes the clock has passed it.
pub const LIFECYCLE_SPAN: Duration = Duration::from_millis(500);

/// The most event buses one account may hold at once IN ONE REGION.
///
/// `CreateEventBus` models `LimitExceededException` — "The request would exceed a
/// service quota for the account" — so a quota is the modelled answer rather than
/// an invented one. EventBridge publishes 100 event buses per account per Region
/// as its default quota; this emulator applies that number per region.
pub const MAX_EVENT_BUSES: usize = 100;

/// How many regions one emulator process keeps a bus registry for.
///
/// The region a request addresses comes off the wire (the SigV4 credential
/// scope), so the NUMBER of per-region quota buckets is caller-chosen: the
/// per-region quota alone bounds each bucket and not the registry. This bounds
/// the buckets, and is the emulator's own limit rather than an AWS rule — well
/// above the handful of regions any one test or client session uses.
pub const MAX_EMULATED_REGIONS: usize = 10;

/// The most event buses one emulator process holds across every region, which is
/// what actually bounds [`ComAmazonawsEventbridgev2Awseventsv2State::event_buses`]
/// (and with it every replay record, because a client token is held by the bus it
/// minted rather than in a table of its own). A create past it answers the same
/// modelled `LimitExceededException` as the per-region quota.
pub const MAX_EVENT_BUSES_PER_EMULATOR: usize = MAX_EVENT_BUSES * MAX_EMULATED_REGIONS;

/// The retention period an event bus holds when the create that minted it named
/// no `StorageConfiguration` (or one carrying no `RetentionPeriodInDays`).
///
/// A bus ALWAYS has an effective storage configuration — `StorageConfigurationOutput`
/// is documented as "Event storage configuration of an event bus, as returned on
/// reads" — so an unspecified retention period is defaulted on the bus rather than
/// left unset. Recorded behaviour puts that default at one day, which is also the
/// minimum of `RetentionPeriodInDays`'s modelled `@range 1..365`.
pub const DEFAULT_RETENTION_PERIOD_IN_DAYS: i32 = 1;

/// The most events one bus keeps.
///
/// This is the EMULATOR's memory bound, not an AWS rule: a configured retention
/// period is a day or more, so it expires nothing within a run, and without a cap
/// any caller could grow one process's heap without limit. Reaching it does not
/// fail a publish; the oldest events simply stop being readable.
pub const MAX_STORED_EVENTS_PER_BUS: usize = 10_000;

/// The whole `Message` of a `ResourceAlreadyExistsException`, wherever this
/// service raises it: a name collision on an event bus, an event source or a
/// subscriber.
///
/// The message of this error class is FIXED text — the service names neither the
/// colliding name nor the resource that already holds it, so nothing of the
/// request or of the store is interpolated into it, and the identity of the
/// existing resource is read with a describe or a list rather than parsed out of
/// an error. It is named once here because every handler that refuses a
/// collision must word it the same way.
pub const RESOURCE_ALREADY_EXISTS_MESSAGE: &str = "Resource already exists";

/// The whole `Message` of the `ResourceNotFoundException` answered for a value
/// that addresses no resource of this service at all.
///
/// FIXED text of that error class: it names neither the value submitted nor any
/// resource, because there is no resource to name — the request named nothing
/// this service ever handed out. A refusal that DID resolve to a resource of
/// this service and found it gone names that resource instead (see
/// [`event_bus_not_found`]).
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource does not exist";

/// How many retired bus ARNs one store remembers (see
/// [`ComAmazonawsEventbridgev2Awseventsv2State::retired_bus_arns`]).
///
/// The EMULATOR's own memory bound, not an AWS rule: the live registry is
/// bounded by [`MAX_EVENT_BUSES_PER_EMULATOR`], but a caller that creates and
/// deletes buses in a loop retires an unbounded number of them, so the record of
/// what this store once minted is kept to the same order of magnitude as the
/// registry itself and the oldest entry is forgotten first.
pub const MAX_RETIRED_BUS_ARNS: usize = MAX_EVENT_BUSES_PER_EMULATOR;

/// Lifecycle state of a stored event bus, as the `BusState` enum words it on the
/// wire. Only the states this slice can produce are modelled here; the failure
/// states (`CREATE_FAILED`, `UPDATE_FAILED`, `DELETE_FAILED`) and `UPDATING`
/// belong to operations that are not implemented yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusLifecycleState {
    /// `CREATING` — provisioning is in progress.
    Creating,
    /// `ACTIVE` — fully operational and able to accept events.
    Active,
    /// `DELETING` — deletion is actively in progress.
    Deleting,
}

impl BusLifecycleState {
    /// The `BusState` member value this state serializes as. Each is an entry of
    /// [`crate::ops::types::BUS_STATE_VALUES`].
    pub fn wire_value(self) -> &'static str {
        match self {
            Self::Creating => "CREATING",
            Self::Active => "ACTIVE",
            Self::Deleting => "DELETING",
        }
    }
}

/// One event accepted by a publish — `PutEvents` or `PutRawEvents` — kept in
/// publication order on its bus so a later reader or subscriber slice can
/// consume the log.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    /// The service-assigned `EventId` reported back for this entry.
    pub event_id: String,
    /// Position of this event in its bus's log, reported as `SequenceNumber`.
    pub sequence_number: u64,
    /// What this event carries, in the form the publish that accepted it sent
    /// it.
    pub payload: EventPayload,
    /// When the event occurred: the entry's `Time` when it carried one, else
    /// the instant the service received it.
    pub time: SystemTime,
    pub received_at: SystemTime,
    /// The `DeduplicationId` the publishing entry carried, kept as sent.
    pub deduplication_id: Option<String>,
    /// The key this event is indexed under in
    /// [`EventBus::deduplicated_events`], when the request asked for
    /// deduplication at all: a digest of the event content under
    /// `CONTENT_BASED`, otherwise the caller's own `DeduplicationId`.
    pub deduplication_key: Option<String>,
    pub event_group_id: Option<String>,
}

impl StoredEvent {
    /// What this event carries when a `PutEvents` entry published it, or `None`
    /// for one a raw publish did.
    ///
    /// The ONE place that reads the structured half of [`EventPayload`] out of
    /// a stored event, so a reader cannot take a raw event for a structured one
    /// by reaching for a member it does not have.
    pub fn structured(&self) -> Option<&StructuredPayload> {
        match &self.payload {
            EventPayload::Structured(structured) => Some(structured),
            EventPayload::Raw(_) => None,
        }
    }

    /// What this event carries when a `PutRawEvents` entry published it, or
    /// `None` for one `PutEvents` did.
    pub fn raw(&self) -> Option<&RawPayload> {
        match &self.payload {
            EventPayload::Raw(raw) => Some(raw),
            EventPayload::Structured(_) => None,
        }
    }
}

/// What one stored event CARRIES, in the form the publish that accepted it
/// sent it.
///
/// This service publishes an event in two forms and a subscriber reads the same
/// three parts out of either: `PutEvents` describes the event with a `Source`, a
/// `DetailType`, `Resources` and a JSON `Detail`, while `PutRawEvents`
/// "publishes pre-shaped events" — the bytes themselves, the `ContentType`
/// naming their form, and the metadata the publisher defined. The two forms are
/// one enum rather than one struct with the other form's members left empty, so
/// no reader can take a raw event for a structured one.
#[derive(Debug, Clone)]
pub enum EventPayload {
    /// A `PutEvents` entry.
    Structured(StructuredPayload),
    /// A `PutRawEvents` entry.
    Raw(RawPayload),
}

/// What a `PutEvents` entry carries: the members its publisher described the
/// event with, kept as sent.
#[derive(Debug, Clone)]
pub struct StructuredPayload {
    pub source: String,
    pub detail_type: String,
    /// The entry's `Detail` — "the event payload, as a valid JSON string" — or
    /// `None` for an entry that published none.
    pub detail: Option<String>,
    /// The ARNs of the resources the event concerns, as the entry listed them.
    pub resources: Vec<String>,
}

/// What a `PutRawEvents` entry carries: "pre-shaped" bytes, the content type
/// naming their form, and the metadata the publisher defined.
#[derive(Debug, Clone)]
pub struct RawPayload {
    /// The entry's `Data` — "the event data as a base64-encoded blob. Supports
    /// binary formats." — held as the BYTES it decoded to, which is what the
    /// publisher sent.
    pub data: Vec<u8>,
    /// The entry's `ContentType`: "content type of the event data".
    pub content_type: String,
    /// The entry's own `Metadata` — "metadata key-value pairs you define" — in
    /// key order, because a map carries no order on the wire.
    pub metadata: BTreeMap<String, String>,
}

/// A stored event bus. Every member of `CreateEventBusResponse`,
/// `DescribeEventBusResponse` and `EventBusSummary` is either a field here or
/// computed from one.
#[derive(Debug, Clone)]
pub struct EventBus {
    pub arn: String,
    pub name: String,
    pub account_id: String,
    pub region: String,
    pub description: Option<String>,
    pub encryption_configuration: Option<EncryptionConfiguration>,
    /// The bus's EFFECTIVE retention period in days: what its create asked for,
    /// or [`DEFAULT_RETENTION_PERIOD_IN_DAYS`] when it asked for nothing. Every
    /// bus has one, which is why this is not optional.
    ///
    /// `StorageConfigurationOutput` pairs it with a `RetentionWindowStartTime`
    /// derived from the reading clock, which is why reads project this field
    /// through [`EventBus::storage_configuration_output`] instead of storing a
    /// window start that would go stale.
    pub retention_period_in_days: i32,
    pub state: BusLifecycleState,
    pub state_reason: Option<String>,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub tags: BTreeMap<String, String>,
    pub events: Vec<StoredEvent>,
    /// Deduplication key -> the `(EventId, SequenceNumber)` of the event that
    /// first carried it, so a deduplication check is a lookup rather than a
    /// scan of the whole log per request entry. The key is whatever the request
    /// deduplicates by (see [`StoredEvent::deduplication_key`]). Written only
    /// beside a push onto [`Self::events`], which is never shortened.
    pub deduplicated_events: BTreeMap<String, (String, u64)>,
    /// The `ClientToken` the create that minted this bus carried, if any.
    pub client_token: Option<String>,
    /// The named resource policies attached to this bus, keyed by `PolicyName`,
    /// so a policy-addressed read is a lookup and an enumeration is in a stable
    /// order. Bounded by the two names the service admits ([`POLICY_NAMES`]).
    pub resource_policies: BTreeMap<String, ResourcePolicy>,
    /// When the clock owes this bus its next lifecycle transition: `CREATING` ->
    /// `ACTIVE`, or `DELETING` -> gone. `None` once the bus has settled.
    pub transition_due_at: Option<SystemTime>,
    /// Sequence number the next accepted event takes.
    pub next_sequence_number: u64,
}

impl EventBus {
    /// The earliest instant from which this bus's stored events are still
    /// available — `RetentionWindowStartTime`.
    ///
    /// Never earlier than the bus's own creation: events from before the bus
    /// existed cannot have been stored.
    pub fn retention_window_start(&self, now: SystemTime) -> SystemTime {
        let days = self.retention_period_in_days;
        let window = Duration::from_secs(u64::from(days.unsigned_abs()).saturating_mul(86_400));
        now.checked_sub(window)
            .filter(|start| *start > self.creation_time)
            .unwrap_or(self.creation_time)
    }

    /// The storage configuration as reads report it: the bus's effective
    /// retention period plus the derived `RetentionWindowStartTime`. Every bus
    /// has one, so a read always reports it.
    pub fn storage_configuration_output(&self, now: SystemTime) -> StorageConfigurationOutput {
        StorageConfigurationOutput {
            retention_period_in_days: Some(self.retention_period_in_days),
            retention_window_start_time: Some(to_date_time(self.retention_window_start(now))),
        }
    }

    /// The free-text description as a READ of this bus reports it: what the bus
    /// holds, and the EMPTY string for a bus holding none.
    ///
    /// `Description` is modelled `@length 0..512`, so the empty string is a legal
    /// value of the member, and a read reports a bus with no description as
    /// having an empty one rather than dropping the member — which is what lets a
    /// client round-trip a read into an update, or render the field, without
    /// distinguishing "absent" from "empty". The CREATE response is projected from
    /// the request as submitted and so carries the member only when the create
    /// named one.
    pub fn reported_description(&self) -> String {
        self.description.clone().unwrap_or_default()
    }

    /// Drop the events the configured retention period has already carried past
    /// its window — "events older than this have expired from retention", per
    /// `RetentionWindowStartTime`'s own documentation.
    ///
    /// Owed by the CLOCK: each of these events expired at a fixed instant whether
    /// or not anybody called, so materialising it on the next call that observes
    /// the store is what makes the reported window describe the stored log.
    pub fn expire_retained_events(&mut self, now: SystemTime) {
        let window_start = self.retention_window_start(now);
        // The log is in receipt order, so the expired events are a prefix.
        let expired = self
            .events
            .iter()
            .take_while(|stored| stored.received_at < window_start)
            .count();
        self.forget_oldest(expired);
    }

    /// Keep the stored log within [`MAX_STORED_EVENTS_PER_BUS`] by dropping the
    /// oldest events, AND the deduplication index with it: the index is pruned
    /// by [`Self::forget_oldest`] in exact correspondence with the log, so it is
    /// bounded by the same constant and can never name an event the log no
    /// longer holds.
    ///
    /// This is the emulator's own memory bound, not an AWS rule: without it any
    /// caller could grow one process's heap without limit, since a retention
    /// period of a day or more expires nothing in the lifetime of a run.
    /// Publication still succeeds; only events already past the cap stop being
    /// readable.
    pub fn trim_to_capacity(&mut self) {
        let excess = self.events.len().saturating_sub(MAX_STORED_EVENTS_PER_BUS);
        self.forget_oldest(excess);
        debug_assert!(
            self.deduplicated_events.len() <= self.events.len(),
            "the deduplication index must not outlive the log it indexes"
        );
    }

    /// Forget the `count` oldest events, keeping [`Self::deduplicated_events`] in
    /// exact correspondence with the log: an event's index entry is dropped with
    /// the event, so the index neither outgrows the log nor reports an event the
    /// log has forgotten.
    fn forget_oldest(&mut self, count: usize) {
        let count = count.min(self.events.len());
        if count == 0 {
            return;
        }
        let dropped: Vec<Option<String>> = self
            .events
            .drain(..count)
            .map(|stored| stored.deduplication_key)
            .collect();
        for id in dropped.into_iter().flatten() {
            self.deduplicated_events.remove(&id);
        }
    }

    /// Whether this bus can accept events right now. Only `ACTIVE` can: the
    /// `InvalidStateException` doc names "an event bus that is still being
    /// created cannot accept events".
    pub fn accepts_events(&self) -> bool {
        self.state == BusLifecycleState::Active
    }
}

/// Per-service state for com-amazonaws-eventbridgev2-awseventsv2. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsEventbridgev2Awseventsv2State {
    /// Buses keyed by `EventBusArn`, so an ARN-addressed read is O(log n) and
    /// enumeration is in a stable order.
    pub event_buses: BTreeMap<String, EventBus>,
    /// EventSources keyed by `EventSourceArn`, so an ARN-addressed read is
    /// O(log n) and enumeration is in a stable order.
    pub event_sources: BTreeMap<String, EventSource>,
    /// Subscribers keyed by `SubscriberArn`, so an ARN-addressed read is
    /// O(log n) and enumeration is in a stable order.
    pub subscribers: BTreeMap<String, Subscriber>,
    /// The ARNs of buses this store MINTED and has since retired, oldest first
    /// and bounded by [`MAX_RETIRED_BUS_ARNS`].
    ///
    /// An `EventBusArn` carries the identifier segment the service minted with
    /// the bus, so an ARN naming an identifier this store never handed out
    /// addresses no resource of this service — which is a different refusal from
    /// one addressed at a bus that is gone. This is what tells the two apart
    /// after the bus itself has been forgotten; see
    /// [`Self::bus_read_refusal`].
    pub retired_bus_arns: VecDeque<String>,
    /// How many service-assigned identifiers this store has handed out.
    ///
    /// This is what makes every minted id UNIQUE: [`Self::next_id_seed`] hands
    /// out each value once, and [`new_bus_id`] / [`new_event_id`] turn it into
    /// an id by a mapping that is injective in the seed, so two ids can never
    /// coincide however the clock behaves.
    pub minted_ids: u64,
}

impl ComAmazonawsEventbridgev2Awseventsv2State {
    /// Materialise every bus lifecycle transition the clock has already made
    /// due, and nothing else.
    ///
    /// Each transition was owed at its stored `transition_due_at` whether or not
    /// anybody called, so applying it on the next call that observes the store
    /// is what makes `CREATING` -> `ACTIVE` and `DELETING` -> gone visible
    /// without a timer. Call this once at the top of every handler, before
    /// reading or writing a bus.
    pub fn settle_lifecycles(&mut self, now: SystemTime) {
        let mut completed_deletions = Vec::new();
        for (arn, bus) in &mut self.event_buses {
            // Owed by the clock too: an event's retention expires at a fixed
            // instant whether or not anybody called.
            bus.expire_retained_events(now);
            let Some(due) = bus.transition_due_at else {
                continue;
            };
            if now < due {
                continue;
            }
            match bus.state {
                BusLifecycleState::Creating => {
                    bus.state = BusLifecycleState::Active;
                    bus.state_reason = None;
                    // The transition happened when it came due, not when it was
                    // observed.
                    bus.last_modified_time = due;
                    bus.transition_due_at = None;
                }
                BusLifecycleState::Deleting => completed_deletions.push(arn.clone()),
                BusLifecycleState::Active => bus.transition_due_at = None,
            }
        }
        for arn in completed_deletions {
            self.event_buses.remove(&arn);
            // The bus is gone, but this store minted its identifier: remember
            // the ARN so a later read of it is refused as the bus that is not
            // there rather than as a value naming no resource at all.
            self.retire_bus_arn(arn.clone());
            // An EventSource forwards onto one bus, so it goes with the bus.
            self.forget_event_sources_of_bus(&arn);
            // A subscriber is created on one bus, so it goes with the bus too.
            self.forget_subscribers_of_bus(&arn);
        }
    }

    /// The bus with this ARN, or `None` when no such bus exists.
    pub fn bus(&self, arn: &str) -> Option<&EventBus> {
        self.event_buses.get(arn)
    }

    /// Remember `arn` as a bus this store minted and has retired, forgetting the
    /// oldest such ARN once [`MAX_RETIRED_BUS_ARNS`] are held.
    fn retire_bus_arn(&mut self, arn: String) {
        if self.retired_bus_arns.iter().any(|retired| *retired == arn) {
            return;
        }
        while self.retired_bus_arns.len() >= MAX_RETIRED_BUS_ARNS {
            self.retired_bus_arns.pop_front();
        }
        self.retired_bus_arns.push_back(arn);
    }

    /// Whether `arn` names an identifier this store handed out: the ARN of a bus
    /// it holds, or of one it minted and has since retired.
    pub fn minted_bus_arn(&self, arn: &str) -> bool {
        self.event_buses.contains_key(arn)
            || self.retired_bus_arns.iter().any(|retired| retired == arn)
    }

    /// The `ResourceNotFoundException` a READ addressed at the whole
    /// `EventBusArn` answers when the store holds no bus there.
    ///
    /// A read resolves the ARN it is given, identifier segment included: an ARN
    /// whose identifier this service minted names a bus that is simply not there
    /// and is refused as that bus ([`event_bus_not_found`]), while an ARN naming
    /// an identifier the service never handed out addresses no resource of this
    /// service and takes the fixed text of the class
    /// ([`RESOURCE_NOT_FOUND_MESSAGE`]).
    pub fn bus_read_refusal(&self, arn: &str) -> ResourceNotFoundException {
        if self.minted_bus_arn(arn) {
            event_bus_not_found(arn)
        } else {
            resource_not_found()
        }
    }

    /// The bus carrying this name in this region. An event-bus name is unique
    /// per account and region, so at most one bus matches — which is why the
    /// region is part of the lookup rather than only of the ARN. A scan is
    /// enough at an emulator's scale and cannot drift out of step with
    /// [`Self::event_buses`] the way a second index could.
    pub fn bus_by_name(&self, region: &str, name: &str) -> Option<&EventBus> {
        self.event_buses
            .values()
            .find(|bus| bus.region == region && bus.name == name)
    }

    /// The bus a create carrying this client token in this region minted, or
    /// `None` when no such bus is stored — which is what makes a retry replay
    /// the original bus instead of minting a second one.
    ///
    /// The record is the BUS: a client token is read off the bus that carried
    /// it, so the replay index is the registry itself. There is no table beside
    /// it that a caller could grow by creating and deleting under a fresh token
    /// each time, and no record can outlive the resource it replays.
    pub fn bus_by_client_token(&self, region: &str, client_token: &str) -> Option<&EventBus> {
        self.event_buses.values().find(|bus| {
            bus.region == region && bus.client_token.as_deref() == Some(client_token)
        })
    }

    /// The next identifier seed, handed out exactly once.
    ///
    /// Call it once per id a handler is about to mint; the id itself is minted
    /// by [`new_bus_id`] or [`new_event_id`] from the seed it returns. A seed
    /// that goes unused (an entry the handler then refuses) costs nothing but
    /// the counter advancing.
    pub fn next_id_seed(&mut self) -> u64 {
        self.minted_ids = self.minted_ids.wrapping_add(1);
        self.minted_ids
    }

    /// How many buses this region holds, which is what [`MAX_EVENT_BUSES`]
    /// bounds: the published quota is per account and Region.
    pub fn buses_in_region(&self, region: &str) -> usize {
        self.event_buses
            .values()
            .filter(|bus| bus.region == region)
            .count()
    }
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsEventbridgev2Awseventsv2State>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsEventbridgev2Awseventsv2State>)
}

/// The region this request was signed for, as the ARNs it mints must carry.
///
/// Read from the SigV4 credential scope, which is where a request names its
/// region. A scope region that is not shaped like an AWS region cannot appear in
/// a modelled ARN (`[a-z][a-z0-9]*(-[a-z0-9]+)*`), so anything else falls back
/// to [`DEFAULT_REGION`] rather than minting an ARN the model rejects.
pub fn request_region(ctx: &smithy_server_runtime::OperationContext<'_>) -> String {
    ::emulator_server::sigv4_credential_scope(&ctx.parts().headers)
        .map(|scope| scope.region)
        .filter(|region| is_region_shaped(region))
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// Whether `region` matches the region segment the `EventBusArn` pattern
/// allows: `[a-z][a-z0-9]*(-[a-z0-9]+)*`.
fn is_region_shaped(region: &str) -> bool {
    region.starts_with(|c: char| c.is_ascii_lowercase())
        && region.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Mint the ARN of a bus: `arn:aws:events:<region>:<account>:event-busv2/<name>/<id>`,
/// the form the `EventBusArn` pattern pins.
pub fn event_bus_arn(region: &str, name: &str, bus_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("{ARN_RESOURCE_TYPE}/{name}/{bus_id}"),
    )
}

/// The bus id for `seed`: [`BUS_ID_LEN`] lowercase base-36 digits, as the
/// `EventBusArn` pattern's `[a-z0-9]{25}` segment requires.
///
/// Base 36 rather than hex because hex would only fill 32 of the pattern's
/// alphabet; 36^25 > 2^128, so the 128 bits [`spread`] produces are rendered
/// whole. `seed` comes from
/// [`ComAmazonawsEventbridgev2Awseventsv2State::next_id_seed`], and both
/// [`spread`] and [`base36`] are injective, so no two buses of one store can
/// carry the same id.
pub fn new_bus_id(seed: u64, now: SystemTime) -> String {
    base36(spread(seed, now), BUS_ID_LEN)
}

/// The `EventId` for `seed`: the same 128 bits rendered as a hyphenated
/// lowercase UUID (8-4-4-4-12), which is the form EventBridge assigns an event
/// id in and so the form a client that parses or correlates one accepts.
///
/// Every one of the 128 bits is kept, so the id stays distinct per event for the
/// reason [`new_bus_id`] gives: the rendering is injective and the bits are.
pub fn new_event_id(seed: u64, now: SystemTime) -> String {
    new_uuid_id(seed, now)
}

/// The 128 bits for `seed` rendered as a hyphenated lowercase UUID (8-4-4-4-12)
/// — the ONE such rendering of this service's opaque server-assigned
/// identifiers, so two of them cannot come to differ in form.
///
/// Every one of the 128 bits is kept, so an id stays distinct for the reason
/// [`new_bus_id`] gives: the rendering is injective and the bits are.
pub fn new_uuid_id(seed: u64, now: SystemTime) -> String {
    ::uuid::Uuid::from_u128(spread(seed, now)).to_string()
}

/// The NAME of the bus an `EventBusArn` addresses: the first segment of the
/// resource id the modelled pattern pins as `event-busv2/<name>/<id>`.
///
/// `None` for a value that is not an ARN of this service's bus resource, which
/// the modelled `@pattern` on every `EventBusArn` member already refuses before
/// a handler runs.
pub fn bus_name_of_arn(event_bus_arn: &str) -> Option<&str> {
    ::service_common::arn::resource_id_of_type(event_bus_arn, ARN_RESOURCE_TYPE)?
        .split('/')
        .next()
        .filter(|name| !name.is_empty())
}

/// The `ResourceNotFoundException` answered for a value that addresses no
/// resource of this service at all: the fixed text of that error class
/// ([`RESOURCE_NOT_FOUND_MESSAGE`]), naming nothing of the request.
///
/// ONE construction, so no two operations word that refusal differently.
pub fn resource_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(RESOURCE_NOT_FOUND_MESSAGE.to_string()),
    }
}

/// The `ResourceNotFoundException` a bus-addressed operation that resolves a bus
/// BY NAME answers with when the store holds no such bus.
///
/// ONE construction, so no two operations word the same refusal differently: a
/// fixed sentence plus a single variable part, the bus NAME the request
/// addressed. The generated id segment and the rest of the ARN are not part of
/// the message — what a client reads back out of it is the name it asked for. A
/// value that names no bus resource at all (which the modelled `@pattern`
/// refuses before a handler runs) is reported as submitted rather than dropped.
pub fn event_bus_not_found(event_bus_arn: &str) -> ResourceNotFoundException {
    let name = bus_name_of_arn(event_bus_arn).unwrap_or(event_bus_arn);
    ResourceNotFoundException {
        message: Some(format!("Event bus not found: {name}")),
    }
}

/// The `ResourceNotFoundException` an operation addressing a REVOCABLE resource
/// answers with when the store holds nothing at the requested ARN.
///
/// `RevocableResourceArn` admits exactly two resource types — a subscriber and an
/// EventSource — and an ARN names which one it addresses in its resource-type
/// segment, so the refusal names the kind of resource the caller asked about
/// rather than a generic one. Each kind is worded by the one construction its
/// own registry owns, so an operation reaching across both registries cannot
/// word the refusal differently from the read of that resource.
pub fn revocable_resource_not_found(arn: &str) -> ResourceNotFoundException {
    if ::service_common::arn::resource_type(arn) == Some(ARN_RESOURCE_TYPE_SUBSCRIBER) {
        subscriber_not_found(arn)
    } else {
        event_source_not_found(arn)
    }
}

/// The retention period a request asking for `requested` leaves the bus on: the
/// `RetentionPeriodInDays` it named, or [`DEFAULT_RETENTION_PERIOD_IN_DAYS`] when
/// it named none.
///
/// ONE place decides what an unspecified retention period means, so the bus a
/// create stores and the configuration a later read reports cannot disagree.
pub fn effective_retention_period(requested: Option<&StorageConfiguration>) -> i32 {
    requested
        .and_then(|configured| configured.retention_period_in_days)
        .unwrap_or(DEFAULT_RETENTION_PERIOD_IN_DAYS)
}

/// 128 bits for one identifier: `seed` spread over the whole range, with the
/// instant it was minted at in the high half so ids of one store do not read as
/// a counter.
///
/// INJECTIVE in `seed`, which is what carries uniqueness: the low half is
/// [`mix64`] of the seed and `mix64` is a bijection on `u64`, so distinct seeds
/// give distinct ids whatever the clock does.
fn spread(seed: u64, now: SystemTime) -> u128 {
    let nanos = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // The low 64 bits of the instant are enough to spread the high half; the
    // mask keeps this total rather than relying on a lossy cast.
    let minted_at = u64::try_from(nanos & u128::from(u64::MAX)).unwrap_or_default();
    let high = mix64(minted_at ^ mix64(seed));
    (u128::from(high) << 64) | u128::from(mix64(seed))
}

/// SplitMix64's finalizer: a BIJECTION on `u64` built from an add, two
/// xor-shift-multiplies by odd constants and a final xor-shift, each of which is
/// itself invertible. Used only to spread a counter over the whole range — never
/// as a hash of caller data, and never where a cryptographic digest is needed.
fn mix64(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `value` as exactly `width` lowercase base-36 digits, zero-padded.
fn base36(mut value: u128, width: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut digits = vec![b'0'; width];
    for slot in digits.iter_mut().rev() {
        *slot = DIGITS[(value % 36) as usize];
        value /= 36;
    }
    String::from_utf8(digits).expect("base-36 digits are ASCII")
}

/// When a lifecycle transition started at `now` comes due.
///
/// Written with `checked_add` because `SystemTime`'s `+` panics on overflow; a
/// clock so far ahead that one more span cannot be represented makes the
/// transition due immediately rather than unwinding a handler.
pub fn lifecycle_due(now: SystemTime) -> SystemTime {
    now.checked_add(LIFECYCLE_SPAN).unwrap_or(now)
}

/// A `SystemTime` as the modelled `Timestamp` type, at the resolution this
/// service reports an instant in: WHOLE epoch seconds.
///
/// Every `Timestamp` member this service answers with travels as an epoch
/// date-time item of the `rpcv2Cbor` protocol, and the instants AWS reports in
/// them carry no sub-second remainder, so the remainder of a server clock
/// reading is dropped here — once, where every read projects an instant — rather
/// than written into a precision no response of this service carries. A client
/// that compares, hashes or round-trips the serialized value therefore reads the
/// same instant back on every read.
pub fn to_date_time(time: SystemTime) -> ::aws_smithy_types::DateTime {
    let since_epoch = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    ::aws_smithy_types::DateTime::from_secs(i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX))
}

/// The modelled `Timestamp` type as a `SystemTime`, for a client-supplied time.
///
/// TOTAL, and deliberately not written with `SystemTime`'s `+`/`-` operators:
/// those panic on overflow, and nothing bounds the magnitude of a `Timestamp` on
/// the wire, so a request carrying a value near `i64::MIN`/`i64::MAX` would
/// unwind inside a handler. `None` means the value is outside the instants this
/// platform can represent, which the caller answers as a refused request rather
/// than by substituting an instant the client did not send.
pub fn from_date_time(value: &::aws_smithy_types::DateTime) -> Option<SystemTime> {
    let whole = Duration::new(value.secs().unsigned_abs(), 0);
    let fraction = Duration::from_nanos(u64::from(value.subsec_nanos()));
    let seconds = if value.secs() >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(whole)?
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(whole)?
    };
    seconds.checked_add(fraction)
}
