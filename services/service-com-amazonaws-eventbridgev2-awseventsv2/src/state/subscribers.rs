// SPDX-License-Identifier: CC0-1.0

//! The subscriber registry: the slice of this service's state that
//! `CreateSubscriber` mints into, `DescribeSubscriber` projects, and `PutEvents`
//! reads to decide where an accepted event goes.
//!
//! A subscriber belongs to one event bus — "Creates a subscriber on an event
//! bus, which delivers matching events to the configured target"
//! (`CreateSubscriber`) — so every stored subscriber names that bus and lives in
//! the bus's region and account. Nothing a response carries is computed from the
//! request alone: every member is read back out of here.
//!
//! This module holds the subscriber and what it DECIDES about an event — which
//! events it is owed ([`Subscriber::delivers`], [`filters_match`]) and what the
//! target is handed ([`delivered_body`]). The call that reaches the target is
//! [`super::subscriber_delivery`].

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use super::{base36, spread, ComAmazonawsEventbridgev2Awseventsv2State, StoredEvent, ARN_SERVICE};
use crate::ops::types::{
    BatchConfiguration, Filter, FilterConfiguration, InvokeConfiguration, LogConfiguration,
    OnFailureConfiguration, PointInTimeConfiguration, ResourceNotFoundException, RetryPolicy,
    Transformer, UniversalTargetParameters,
};

/// The resource-type segment of a `SubscriberArn`, from the modelled pattern
/// `^arn:aws…:events:…:…:subscriber\/<name>\/[a-z0-9]{25}$`.
pub const ARN_RESOURCE_TYPE_SUBSCRIBER: &str = "subscriber";

/// Width of the id segment of a `SubscriberArn`: the modelled pattern pins it at
/// exactly `[a-z0-9]{25}`.
pub const SUBSCRIBER_ID_LEN: usize = 25;

/// The most subscribers one emulator process holds.
///
/// This is the EMULATOR's own bound rather than a published AWS quota: without
/// it any caller could grow one process's heap without limit. A create past it
/// answers the `LimitExceededException` `CreateSubscriber` models — "The request
/// would exceed a service quota for the account".
pub const MAX_SUBSCRIBERS_PER_EMULATOR: usize = 1_000;

/// `SubscriberState::RUNNING`, an entry of
/// [`crate::ops::types::SUBSCRIBER_STATE_VALUES`].
pub const SUBSCRIBER_STATE_RUNNING: &str = "RUNNING";

/// `SubscriberState::STOPPED`, the other entry of
/// [`crate::ops::types::SUBSCRIBER_STATE_VALUES`].
pub const SUBSCRIBER_STATE_STOPPED: &str = "STOPPED";

/// `OrderingType::UNORDERED` — "delivers without an ordering guarantee"
/// (`OrderingType`), the mode a create that names none takes: FIFO delivery is
/// the mode a caller asks for by naming `Type` FIFO and an `EventGroupId` to
/// order within.
pub const ORDERING_TYPE_UNORDERED: &str = "UNORDERED";

/// `StartingPosition::LATEST` — delivery begins at the END of the bus's log as
/// it stands when the subscriber is created, so the subscriber is owed the
/// events published after it exists and none of the bus's backlog. It is the
/// position a create that names none takes.
pub const STARTING_POSITION_LATEST: &str = "LATEST";

/// `StartingPosition::POINT_IN_TIME` — delivery begins at the point of the
/// bus's retained log the subscriber's `PointInTimeConfiguration` names, so the
/// subscriber is owed the backlog from that point on.
pub const STARTING_POSITION_POINT_IN_TIME: &str = "POINT_IN_TIME";

/// `ResumePosition::LAST_PROCESSED` — "Continue delivery from where it stopped
/// (last persisted progress)", so an unpause from this position leaves the
/// subscriber's own position on its bus's log alone and the backlog it accrued
/// while stopped is still owed to it. It is the position an unpause naming none
/// takes ("Defaults to LAST_PROCESSED when omitted"). An entry of
/// [`crate::ops::types::RESUME_POSITION_VALUES`].
pub const RESUME_POSITION_LAST_PROCESSED: &str = "LAST_PROCESSED";

/// `ResumePosition::LATEST` — "Skip the backlog accrued while stopped; deliver
/// only new events (resume at the tip)", so an unpause from this position moves
/// the subscriber's position to the END of its bus's log and nothing published
/// while it was stopped is delivered. An entry of
/// [`crate::ops::types::RESUME_POSITION_VALUES`].
pub const RESUME_POSITION_LATEST: &str = "LATEST";

/// `PointType::HORIZON` — the point is the horizon of the bus's retained log:
/// the oldest event the bus still holds. An entry of
/// [`crate::ops::types::POINT_TYPE_VALUES`].
pub const POINT_TYPE_HORIZON: &str = "HORIZON";

/// `PointType::TIMESTAMP` — the point is the instant `StartingPoint` names
/// ("Timestamp to start from. Required when PointType is TIMESTAMP"). An entry
/// of [`crate::ops::types::POINT_TYPE_VALUES`].
pub const POINT_TYPE_TIMESTAMP: &str = "TIMESTAMP";

/// `FilterScope::DATA` — the filter is matched against the event's own payload.
/// An entry of [`crate::ops::types::FILTER_SCOPE_VALUES`].
pub const FILTER_SCOPE_DATA: &str = "DATA";

/// `FilterScope::METADATA` — the filter is matched against the metadata the
/// publisher sent with the event. An entry of
/// [`crate::ops::types::FILTER_SCOPE_VALUES`].
pub const FILTER_SCOPE_METADATA: &str = "METADATA";

/// `FilterScope::SYSTEM_METADATA` — the filter is matched against the event's
/// system metadata. An entry of
/// [`crate::ops::types::FILTER_SCOPE_VALUES`].
pub const FILTER_SCOPE_SYSTEM_METADATA: &str = "SYSTEM_METADATA";

/// `TransformerType::RAW` — the target is handed the event itself, untouched by
/// any transformation ([`DeliveredEvent::untransformed`]): the envelope
/// EventBridge writes around a structured entry's detail, or a raw entry's
/// pre-shaped payload as its publisher sent it. It is also what a subscriber
/// that configures no transformer hands its target. An entry of
/// [`crate::ops::types::TRANSFORMER_TYPE_VALUES`].
pub const TRANSFORMER_TYPE_RAW: &str = "RAW";

/// `TransformerType::WITH_METADATA` — the target is handed the event inside
/// this service's own metadata envelope ([`DeliveredEvent::document`]): the
/// event's identity and position on its bus's log beside the three parts this
/// service models an event as. An entry of
/// [`crate::ops::types::TRANSFORMER_TYPE_VALUES`].
pub const TRANSFORMER_TYPE_WITH_METADATA: &str = "WITH_METADATA";

/// The member of a delivered event envelope carrying the envelope's own schema
/// version, which is the fixed text [`ENVELOPE_VERSION_VALUE`].
const ENVELOPE_VERSION: &str = "version";

/// The one version an event envelope is written at.
const ENVELOPE_VERSION_VALUE: &str = "0";

/// The member of a delivered event envelope carrying the event's identity — the
/// `EventId` `PutEvents` reported for it.
const ENVELOPE_ID: &str = "id";

/// The member of a delivered event envelope carrying the publishing entry's
/// `DetailType`.
const ENVELOPE_DETAIL_TYPE: &str = "detail-type";

/// The member of a delivered event envelope carrying the publishing entry's
/// `Source`.
const ENVELOPE_SOURCE: &str = "source";

/// The member of a delivered event envelope carrying the account the bus the
/// event was published to belongs to.
const ENVELOPE_ACCOUNT: &str = "account";

/// The member of a delivered event envelope carrying when the event occurred.
const ENVELOPE_TIME: &str = "time";

/// The member of a delivered event envelope carrying the region of the bus the
/// event was published to.
const ENVELOPE_REGION: &str = "region";

/// The member of a delivered event envelope carrying the publishing entry's
/// `Resources`.
const ENVELOPE_RESOURCES: &str = "resources";

/// The member of a delivered event envelope carrying the event's payload — the
/// publishing entry's `Detail` as a document.
const ENVELOPE_DETAIL: &str = "detail";

/// The member of a delivered event that carries the event's payload — the
/// `DATA` a filter of that scope is matched against.
pub const DELIVERED_DATA: &str = "Data";

/// The member of a delivered event that carries the publisher's metadata — the
/// `METADATA` a filter of that scope is matched against.
pub const DELIVERED_METADATA: &str = "Metadata";

/// The member of a delivered event that carries the event's system metadata —
/// the `SYSTEM_METADATA` a filter of that scope is matched against.
pub const DELIVERED_SYSTEM_METADATA: &str = "SystemMetadata";

/// The prefix the SERVICE's own system-metadata members carry, which is
/// reserved: `EventMetadataKey` admits any key but a publisher's own members
/// live in `Metadata`, so a member the service stamps onto an event's system
/// metadata is namespaced and cannot collide with one a publisher sent.
pub const SYSTEM_METADATA_PREFIX: &str = "aws:";

/// The system-metadata member carrying the `EventId` the publish reported for
/// this event — the event's identity, as a delivered event carries it.
pub const SYSTEM_EVENT_ID: &str = "aws:EventId";

/// The system-metadata member carrying the `SequenceNumber` the publish
/// reported for this event — its position on its bus's log.
pub const SYSTEM_SEQUENCE_NUMBER: &str = "aws:SequenceNumber";

/// The system-metadata member saying whether this delivery is of an event
/// published while the subscriber was watching, or of its bus's retained
/// backlog ([`DELIVERY_TYPE_LIVE`] / [`DELIVERY_TYPE_REPLAY`]).
pub const SYSTEM_DELIVERY_TYPE: &str = "aws:DeliveryType";

/// The [`SYSTEM_DELIVERY_TYPE`] of a delivery the bus owed as it accepted the
/// event: the subscriber already existed when the event was received.
pub const DELIVERY_TYPE_LIVE: &str = "LIVE";

/// The [`SYSTEM_DELIVERY_TYPE`] of a delivery out of the bus's RETAINED log: an
/// event the bus had already received when the subscriber was created, which a
/// subscriber reads by starting at a point in time rather than at the end of
/// the log. A consumer that must not act twice on an event it has seen tells
/// the two apart by this member.
pub const DELIVERY_TYPE_REPLAY: &str = "REPLAY";

/// Customer-controlled run state of a stored subscriber, as the
/// `SubscriberState` enum words it on the wire.
///
/// It is the subscriber's OWN state rather than a provisioning state:
/// `StateReason` documents that "EventSources and subscribers are provisioned
/// synchronously and report failures directly on the request", so a created
/// subscriber is operational as soon as the call that minted it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriberRunState {
    /// `RUNNING` — the subscriber delivers matching events to its target.
    Running,
    /// `STOPPED` — the caller has stopped delivery.
    Stopped,
}

impl SubscriberRunState {
    /// The `SubscriberState` member value this state serializes as. Each is an
    /// entry of [`crate::ops::types::SUBSCRIBER_STATE_VALUES`].
    pub fn wire_value(self) -> &'static str {
        match self {
            Self::Running => SUBSCRIBER_STATE_RUNNING,
            Self::Stopped => SUBSCRIBER_STATE_STOPPED,
        }
    }

    /// The state a request naming `value` asks for.
    ///
    /// `None` for a value `SubscriberState` does not declare, which the boundary
    /// validator refuses before a handler runs; a request naming no state at all
    /// takes [`Self::Running`], because a subscriber a caller created to receive
    /// events receives them without a second call.
    pub fn from_wire(value: Option<&str>) -> Option<Self> {
        match value {
            None | Some(SUBSCRIBER_STATE_RUNNING) => Some(Self::Running),
            Some(SUBSCRIBER_STATE_STOPPED) => Some(Self::Stopped),
            Some(_) => None,
        }
    }
}

/// A stored subscriber. Every member of `CreateSubscriberResponse` and
/// `DescribeSubscriberResponse` is either a field here or computed from one.
#[derive(Debug, Clone)]
pub struct Subscriber {
    pub arn: String,
    pub name: String,
    /// The `EventBusArn` of the bus this subscriber is on.
    pub event_bus_arn: String,
    pub account_id: String,
    /// The region this subscriber lives in: the region of its bus, since a bus
    /// belongs to exactly one region and the subscriber is created on that bus.
    pub region: String,
    pub invoke_configuration: InvokeConfiguration,
    pub description: Option<String>,
    pub filter_configuration: Option<FilterConfiguration>,
    /// The `Type` — the delivery ordering mode — as reads report it.
    pub ordering_type: String,
    pub starting_position: String,
    pub point_in_time_configuration: Option<PointInTimeConfiguration>,
    pub batch_configuration: Option<BatchConfiguration>,
    pub transformer: Option<Transformer>,
    pub retry_policy: Option<RetryPolicy>,
    pub on_failure_configuration: Option<OnFailureConfiguration>,
    pub log_configuration: Option<LogConfiguration>,
    pub state: SubscriberRunState,
    /// Whether the bus owner has withdrawn this subscriber, which `Revoked`
    /// reports.
    pub revoked: bool,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub tags: BTreeMap<String, String>,
    /// The `ClientToken` the create that minted this subscriber carried, if any.
    pub client_token: Option<String>,
    /// The position in its bus's log this subscriber delivers from next: the
    /// `SequenceNumber` of the earliest event it has still to be delivered.
    ///
    /// A subscriber CONSUMES its bus's log from the position its
    /// `StartingPosition` put it at ([`starting_sequence_number`]):
    /// `ResumePosition` documents the "position from which delivery resumes when
    /// a subscriber is unpaused", so a subscriber holds a position on the log
    /// rather than only seeing what is published while it watches. The position
    /// advances as the events it is owed are taken
    /// ([`ComAmazonawsEventbridgev2Awseventsv2State::take_due_delivery`]) and
    /// stands still while the subscriber is not delivering, which is what leaves
    /// a stopped subscriber its backlog.
    pub next_sequence_number: u64,
}

impl Subscriber {
    /// Whether this subscriber is owed the events its bus accepts right now.
    ///
    /// "Delivery requires State RUNNING on a subscriber that is not revoked" —
    /// `SubscriberState`'s own documentation — so both halves of that sentence
    /// are read here rather than at each delivery site.
    pub fn delivers(&self) -> bool {
        self.state == SubscriberRunState::Running && !self.revoked
    }

    /// The ARN of the resource this subscriber invokes, as its
    /// `InvokeConfiguration` names it.
    pub fn target_arn(&self) -> &str {
        &self.invoke_configuration.target_arn
    }

    /// The instant past which this subscriber is owed no event — "Timestamp to
    /// stop at. Optional." (`PointInTimeConfiguration$EndPoint`) — or `None`
    /// for one whose delivery is not bounded ahead.
    pub fn end_point(&self) -> Option<SystemTime> {
        self.point_in_time_configuration
            .as_ref()?
            .end_point
            .as_ref()
            .and_then(super::from_date_time)
    }

    /// Whether delivering `stored` to this subscriber is LIVE or a REPLAY.
    ///
    /// A subscriber is owed the part of its bus's log its `StartingPosition`
    /// puts it at ([`starting_sequence_number`]), so an event the bus had
    /// already RECEIVED when the subscriber was created is one out of the
    /// retained log rather than one the bus owed as it arrived — which is what
    /// makes that delivery a replay. Everything received while the subscriber
    /// existed is live, whatever position it started from.
    pub fn delivery_type(&self, stored: &StoredEvent) -> &'static str {
        if stored.received_at < self.creation_time {
            DELIVERY_TYPE_REPLAY
        } else {
            DELIVERY_TYPE_LIVE
        }
    }
}

/// The position on `bus`'s log a subscriber created with this starting position
/// begins delivering from.
///
/// `LATEST` begins at the END of the log as it stands now, so the subscriber is
/// owed what is published after it exists. `POINT_IN_TIME` begins inside the
/// retained log, at the point its `PointInTimeConfiguration` names: the
/// `HORIZON` — the oldest event the bus still holds — or the first event
/// received at or after the `StartingPoint` a `TIMESTAMP` names. A point past
/// every stored event, and one carrying no instant to place, is the end of the
/// log, which owes the subscriber nothing published before it existed.
///
/// The position is by the log's own order (`SequenceNumber`), which is receipt
/// order, so a `TIMESTAMP` is placed by when the bus RECEIVED an event rather
/// than by the `Time` its publisher stated: a client-stated time can precede
/// another event's and would otherwise put the position behind events already
/// delivered.
pub fn starting_sequence_number(
    bus: &super::EventBus,
    starting_position: &str,
    point: Option<&PointInTimeConfiguration>,
) -> u64 {
    let tip = bus.next_sequence_number;
    if starting_position != STARTING_POSITION_POINT_IN_TIME {
        return tip;
    }
    let horizon = bus
        .events
        .first()
        .map_or(tip, |oldest| oldest.sequence_number);
    match point.map(|point| point.point_type.as_str()) {
        // A `POINT_IN_TIME` subscriber carrying no point at all is at the
        // horizon: the whole retained log is the backlog it asked for.
        None | Some(POINT_TYPE_HORIZON) => horizon,
        Some(POINT_TYPE_TIMESTAMP) => {
            let Some(from) = point
                .and_then(|point| point.starting_point.as_ref())
                .and_then(super::from_date_time)
            else {
                return tip;
            };
            bus.events
                .iter()
                .find(|stored| stored.received_at >= from)
                .map_or(tip, |first| first.sequence_number)
        }
        Some(_) => tip,
    }
}

/// How far in the past a `TIMESTAMP` starting point has to lie for the bus to
/// replay from it.
///
/// Recorded behaviour of this operation: AWS refuses a `TIMESTAMP` point newer
/// than this — the most recently received events are not replayable yet — and
/// the refusal it answers STATES the width in words ("at least 5 minutes in the
/// past"), which is where this number comes from rather than from a span this
/// emulator chose. The bound is a property of the service and not of the
/// request, so no member carries it and
/// [`starting_point_too_recent_message`] renders that one sentence from this
/// one constant.
///
/// It is the NEAR side of the replayable window; the far side is the bus's own
/// `RetentionWindowStartTime` ([`super::EventBus::retention_window_start`]), so
/// a bus younger than this bound has no replayable instant yet and a `HORIZON`
/// point is how a caller asks for its log instead.
pub const MIN_STARTING_POINT_AGE: Duration = Duration::from_secs(5 * 60);

/// The whole `Message` of the `InvalidInputException` a create naming a
/// `TIMESTAMP` starting point that has not happened yet is refused with: fixed
/// text naming the member at fault, interpolating no value of the request.
pub const STARTING_POINT_IN_FUTURE_MESSAGE: &str =
    "PointInTimeConfiguration.StartingPoint must not be in the future";

/// The whole `Message` of the `InvalidInputException` a create carrying
/// `PointType` `TIMESTAMP` with no `StartingPoint` is refused with —
/// "Required when PointType is TIMESTAMP", per the member's own documentation.
pub const STARTING_POINT_REQUIRED_MESSAGE: &str =
    "PointInTimeConfiguration.StartingPoint is required when PointType is TIMESTAMP";

/// The whole `Message` of the `InvalidInputException` a create naming a
/// `TIMESTAMP` starting point newer than [`MIN_STARTING_POINT_AGE`] is refused
/// with: fixed text, stating the bound in minutes off that one constant so the
/// refusal cannot name a width the gate does not apply.
pub fn starting_point_too_recent_message() -> String {
    format!(
        "PointInTimeConfiguration.StartingPoint must be at least {} minutes in the past",
        MIN_STARTING_POINT_AGE.as_secs() / 60
    )
}

/// The whole `Message` of the `InvalidInputException` a create naming a
/// `TIMESTAMP` starting point before the bus's retention window is refused
/// with: fixed text around ONE variable part, the `StartingPoint` the REQUEST
/// named, rendered at whole-second UTC ([`starting_point_stamp`]). The window
/// the point falls outside of is the bus's own
/// `StorageConfiguration.RetentionWindowStartTime`, so the message names the
/// value the caller has to correct rather than restating the bus's.
pub fn starting_point_outside_window_message(stamp: &str) -> String {
    format!(
        "PointInTimeConfiguration.StartingPoint ({stamp}) falls outside the bus's retention window"
    )
}

/// A requested `StartingPoint` as the refusal above renders it: RFC 3339 at
/// whole-second precision closed by `Z`, the resolution every `Timestamp` of
/// this service travels at ([`super::to_date_time`]).
pub fn starting_point_stamp(value: &::aws_smithy_types::DateTime) -> String {
    ::aws_smithy_types::DateTime::from_secs(value.secs())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// Why `point` names a position `bus` cannot be replayed from, or `None` when it
/// names one it can.
///
/// A `POINT_IN_TIME` subscriber is placed inside the bus's RETAINED log
/// ([`starting_sequence_number`]), so a `TIMESTAMP` point has to be an instant
/// that log can still answer for: one that has already happened, old enough to
/// have been stored ([`MIN_STARTING_POINT_AGE`]), and no earlier than the
/// window the bus reports as `RetentionWindowStartTime` — "the earliest point in
/// time from which stored events are available. Events older than this have
/// expired from retention". A point outside that range names no reachable
/// position, so the create is refused rather than pinned to one.
///
/// ONE reading of that range, so the gate a create is held to and the window a
/// read of the bus reports cannot disagree. `HORIZON` carries no instant of the
/// request — the bus itself decides where its log begins — and a configuration
/// beside any other starting position is "not applicable", so neither is placed
/// here.
pub fn point_in_time_fault(
    bus: &super::EventBus,
    starting_position: &str,
    point: Option<&PointInTimeConfiguration>,
    now: SystemTime,
) -> Option<String> {
    if starting_position != STARTING_POSITION_POINT_IN_TIME {
        return None;
    }
    let point = point?;
    if point.point_type != POINT_TYPE_TIMESTAMP {
        return None;
    }
    let Some(requested) = point.starting_point.as_ref() else {
        return Some(STARTING_POINT_REQUIRED_MESSAGE.to_string());
    };
    let stamp = starting_point_stamp(requested);
    // An instant outside the instants this platform can represent is outside
    // every bus's window too, and is reported as the value the request named.
    let Some(from) = super::from_date_time(requested) else {
        return Some(starting_point_outside_window_message(&stamp));
    };
    let Ok(age) = now.duration_since(from) else {
        return Some(STARTING_POINT_IN_FUTURE_MESSAGE.to_string());
    };
    if age < MIN_STARTING_POINT_AGE {
        return Some(starting_point_too_recent_message());
    }
    if from < bus.retention_window_start(now) {
        return Some(starting_point_outside_window_message(&stamp));
    }
    None
}

/// Mint the ARN of a subscriber:
/// `arn:aws:events:<region>:<account>:subscriber/<name>/<id>`, the form the
/// `SubscriberArn` pattern pins.
pub fn subscriber_arn(region: &str, name: &str, subscriber_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("{ARN_RESOURCE_TYPE_SUBSCRIBER}/{name}/{subscriber_id}"),
    )
}

/// The subscriber id for `seed`: [`SUBSCRIBER_ID_LEN`] lowercase base-36 digits,
/// as the `SubscriberArn` pattern's `[a-z0-9]{25}` segment requires.
///
/// `seed` comes from
/// [`ComAmazonawsEventbridgev2Awseventsv2State::next_id_seed`], which hands out
/// each value once, and the mapping from seed to id is injective, so no two
/// subscribers of one store can carry the same id.
pub fn new_subscriber_id(seed: u64, now: SystemTime) -> String {
    base36(spread(seed, now), SUBSCRIBER_ID_LEN)
}

/// One accepted event as its bus's subscribers see it: the three parts a filter
/// is scoped to, beside the identity `PutEvents` reported for it.
///
/// The parts are the event's own, as this service models an event:
/// `PutRawEvents` publishes `Data`, `Metadata` and `SystemMetadata`, and
/// `FilterScope` scopes a filter to exactly one of those three. A structured
/// `PutEvents` entry carries the same three parts — its `Detail` is the data,
/// the members the publisher sets beside it are the metadata, and its
/// `SystemMetadata` is the system metadata — so one rendering serves both
/// filtering and delivery.
#[derive(Debug, Clone)]
pub struct DeliveredEvent {
    /// The `EventId` the publish reported for this event.
    pub event_id: String,
    /// The `SequenceNumber` the publish reported for this event.
    pub sequence_number: u64,
    /// The bus this event was published to.
    pub event_bus_arn: String,
    /// The account that bus belongs to.
    pub account_id: String,
    /// The region that bus lives in.
    pub region: String,
    /// This event as the service represents it with NO transformation applied,
    /// as the text a target is handed: for a structured event the envelope this
    /// service writes around a published payload, and for a raw one the
    /// pre-shaped payload as its publisher sent it
    /// ([`crate::state::RawPayload::untransformed_text`]).
    pub untransformed: String,
    /// The event's payload: the `DATA` a filter of that scope is matched
    /// against.
    pub data: ::serde_json::Value,
    /// The metadata the publisher sent with the event.
    pub metadata: ::serde_json::Value,
    /// The event's system metadata.
    pub system_metadata: ::serde_json::Value,
}

/// Render a stored event as THIS subscriber sees it — the account and region
/// are the BUS's, since a subscriber lives in the account and region of the bus
/// it was created on, and whether the delivery is a replay is decided by where
/// the event sits relative to the subscriber's own creation
/// ([`Subscriber::delivery_type`]).
///
/// A structured entry's `Detail` is parsed as a document, because a
/// `DATA`-scoped filter reaches INTO it and a target is handed it as a document
/// rather than as a quoted string; an entry that carried no `Detail`, or one
/// whose text is not a document, has an empty object for its data — the same
/// value a `DATA` clause finds nothing in. A raw entry's parts are read by the
/// payload itself ([`crate::state::RawPayload`]), which is what knows whether
/// its bytes are text this service reads or binary it does not.
pub fn delivered_event(subscriber: &Subscriber, stored: &StoredEvent) -> DeliveredEvent {
    let account_id = subscriber.account_id.as_str();
    let region = subscriber.region.as_str();
    // The system metadata both forms carry: the members
    // `PutEventsSystemMetadata` and `PutRawEventsSystemMetadata` share, beside
    // the service's OWN, which travel under the reserved `aws:` prefix so no
    // publisher-defined member can collide with one.
    let mut system_metadata = ::serde_json::Map::new();
    system_metadata.insert(
        SYSTEM_EVENT_ID.to_string(),
        ::serde_json::Value::String(stored.event_id.clone()),
    );
    // `SequenceNumber` is a string on the wire
    // (`PutEventsResultEntry$SequenceNumber`), so it travels as the same text
    // the publisher was told.
    system_metadata.insert(
        SYSTEM_SEQUENCE_NUMBER.to_string(),
        ::serde_json::Value::String(stored.sequence_number.to_string()),
    );
    system_metadata.insert(
        SYSTEM_DELIVERY_TYPE.to_string(),
        ::serde_json::Value::String(subscriber.delivery_type(stored).to_string()),
    );
    if let Some(group) = &stored.event_group_id {
        system_metadata.insert(
            EVENT_GROUP_ID_METADATA.to_string(),
            ::serde_json::Value::String(group.clone()),
        );
    }
    if let Some(deduplication_id) = &stored.deduplication_id {
        system_metadata.insert(
            DEDUPLICATION_ID_METADATA.to_string(),
            ::serde_json::Value::String(deduplication_id.clone()),
        );
    }

    let (untransformed, data, metadata) = match &stored.payload {
        crate::state::EventPayload::Structured(structured) => {
            let time = to_event_time(stored.time);
            let data = structured
                .detail
                .as_deref()
                .and_then(|detail| crate::state::json_document(detail.as_bytes()))
                .unwrap_or_else(|| ::serde_json::Value::Object(::serde_json::Map::new()));
            let envelope = structured_envelope(
                &stored.event_id,
                account_id,
                region,
                structured,
                &time,
                &data,
            );
            (
                envelope.to_string(),
                data,
                structured_metadata(structured, &time),
            )
        }
        crate::state::EventPayload::Raw(raw) => {
            // "Content type of the event data" is the event's own system
            // metadata, so a `SYSTEM_METADATA` filter is matched against it and
            // a delivery that carries the metadata envelope carries it.
            system_metadata.insert(
                CONTENT_TYPE_METADATA.to_string(),
                ::serde_json::Value::String(raw.content_type.clone()),
            );
            (
                raw.untransformed_text(),
                raw.data_value(),
                raw.metadata_document(),
            )
        }
    };

    DeliveredEvent {
        event_id: stored.event_id.clone(),
        sequence_number: stored.sequence_number,
        event_bus_arn: subscriber.event_bus_arn.clone(),
        account_id: account_id.to_string(),
        region: region.to_string(),
        untransformed,
        data,
        metadata,
        system_metadata: ::serde_json::Value::Object(system_metadata),
    }
}

/// The metadata of a structured event: the members its publisher described the
/// event with beside its `Detail`, which are what a `METADATA`-scoped filter of
/// such an event is matched against.
fn structured_metadata(
    structured: &crate::state::StructuredPayload,
    time: &str,
) -> ::serde_json::Value {
    let mut metadata = ::serde_json::Map::new();
    metadata.insert(
        SOURCE_METADATA.to_string(),
        ::serde_json::Value::String(structured.source.clone()),
    );
    metadata.insert(
        DETAIL_TYPE_METADATA.to_string(),
        ::serde_json::Value::String(structured.detail_type.clone()),
    );
    metadata.insert(
        RESOURCES_METADATA.to_string(),
        ::serde_json::Value::Array(
            structured
                .resources
                .iter()
                .map(|resource| ::serde_json::Value::String(resource.clone()))
                .collect(),
        ),
    );
    metadata.insert(
        TIME_METADATA.to_string(),
        ::serde_json::Value::String(time.to_string()),
    );
    ::serde_json::Value::Object(metadata)
}

/// A structured event as EventBridge represents it on the wire: the envelope the
/// service writes around a published payload.
///
/// The envelope is the event ITSELF rather than one shape of a delivery, so
/// every member of it is the event's own: the envelope's `version` is the one
/// version an envelope is written at, its `id` is the `EventId` the publish
/// reported for the event, its `source`, `detail-type`, `resources` and `detail`
/// are what the publishing entry carried, its `time` is when the event occurred
/// ("Defaults to the time the service receives the event when omitted" —
/// `PutEventsRequestEntry$Time`), and its `account` and `region` are the bus's.
/// An entry that listed no resources carries an empty list, because an event
/// always concerns a (possibly empty) set of them.
fn structured_envelope(
    event_id: &str,
    account_id: &str,
    region: &str,
    structured: &crate::state::StructuredPayload,
    time: &str,
    data: &::serde_json::Value,
) -> ::serde_json::Value {
    let mut envelope = ::serde_json::Map::new();
    envelope.insert(
        ENVELOPE_VERSION.to_string(),
        ::serde_json::Value::String(ENVELOPE_VERSION_VALUE.to_string()),
    );
    envelope.insert(
        ENVELOPE_ID.to_string(),
        ::serde_json::Value::String(event_id.to_string()),
    );
    envelope.insert(
        ENVELOPE_DETAIL_TYPE.to_string(),
        ::serde_json::Value::String(structured.detail_type.clone()),
    );
    envelope.insert(
        ENVELOPE_SOURCE.to_string(),
        ::serde_json::Value::String(structured.source.clone()),
    );
    envelope.insert(
        ENVELOPE_ACCOUNT.to_string(),
        ::serde_json::Value::String(account_id.to_string()),
    );
    envelope.insert(
        ENVELOPE_TIME.to_string(),
        ::serde_json::Value::String(time.to_string()),
    );
    envelope.insert(
        ENVELOPE_REGION.to_string(),
        ::serde_json::Value::String(region.to_string()),
    );
    envelope.insert(
        ENVELOPE_RESOURCES.to_string(),
        ::serde_json::Value::Array(
            structured
                .resources
                .iter()
                .map(|resource| ::serde_json::Value::String(resource.clone()))
                .collect(),
        ),
    );
    envelope.insert(ENVELOPE_DETAIL.to_string(), data.clone());
    ::serde_json::Value::Object(envelope)
}

/// The metadata member carrying a structured entry's `Source`.
const SOURCE_METADATA: &str = "Source";

/// The metadata member carrying a structured entry's `DetailType`.
const DETAIL_TYPE_METADATA: &str = "DetailType";

/// The metadata member carrying a structured entry's `Resources`.
const RESOURCES_METADATA: &str = "Resources";

/// The metadata member carrying a structured entry's `Time`.
const TIME_METADATA: &str = "Time";

/// The system-metadata member carrying an entry's `EventGroupId`, as
/// `PutEventsSystemMetadata` and `PutRawEventsSystemMetadata` both name it.
const EVENT_GROUP_ID_METADATA: &str = "EventGroupId";

/// The system-metadata member carrying an entry's `DeduplicationId`, as
/// `PutEventsSystemMetadata` and `PutRawEventsSystemMetadata` both name it.
const DEDUPLICATION_ID_METADATA: &str = "DeduplicationId";

/// The system-metadata member carrying a raw entry's `ContentType`, as
/// `PutRawEventsSystemMetadata` names it.
const CONTENT_TYPE_METADATA: &str = "ContentType";

impl DeliveredEvent {
    /// The part of this event a filter of `scope` is matched against, or `None`
    /// for a scope this slice does not place — a value `FilterScope` does not
    /// declare, which the boundary validator refuses before a handler stores it.
    pub fn scoped(&self, scope: &str) -> Option<&::serde_json::Value> {
        match scope {
            FILTER_SCOPE_DATA => Some(&self.data),
            FILTER_SCOPE_METADATA => Some(&self.metadata),
            FILTER_SCOPE_SYSTEM_METADATA => Some(&self.system_metadata),
            _ => None,
        }
    }

    /// This event inside the METADATA ENVELOPE a `WITH_METADATA` transformer
    /// names — "deliver the event with metadata envelope" — which is the three
    /// parts this service models an event as: the publisher's metadata, the
    /// event's system metadata and its payload.
    ///
    /// The envelope carries those three and nothing else: a `WITH_METADATA`
    /// delivery recorded against the real service holds exactly `Data`,
    /// `Metadata` and `SystemMetadata`, so the event's own identity travels in
    /// the system metadata the publisher sent rather than as members beside
    /// them.
    pub fn document(&self) -> ::serde_json::Value {
        let mut document = ::serde_json::Map::new();
        document.insert(DELIVERED_METADATA.to_string(), self.metadata.clone());
        document.insert(
            DELIVERED_SYSTEM_METADATA.to_string(),
            self.system_metadata.clone(),
        );
        document.insert(DELIVERED_DATA.to_string(), self.data.clone());
        ::serde_json::Value::Object(document)
    }
}

/// Whether every filter of `configuration` matches `event`.
///
/// "All Filters are implicitly ANDed — an event must match every Filter to be
/// delivered" (`FilterConfiguration`), and each Filter names the `Scope` of the
/// event it reads: `DATA` the payload, `METADATA` the publisher's metadata,
/// `SYSTEM_METADATA` the system metadata ([`DeliveredEvent::scoped`]). A
/// subscriber that configures no filters at all constrains nothing, so every
/// event its bus accepts is one it is owed.
///
/// The pattern is read by the ONE shared EventBridge pattern matcher
/// (`service_common::event_pattern`), so a pattern that selects an event here
/// selects it wherever else this emulator evaluates one, and the work it costs
/// is charged to `budget`.
pub fn filters_match(
    configuration: Option<&FilterConfiguration>,
    event: &DeliveredEvent,
    budget: &mut ::service_common::event_pattern::MatchBudget,
) -> bool {
    let Some(filters) = configuration.and_then(|configuration| configuration.filters.as_deref())
    else {
        return true;
    };
    filters
        .iter()
        .all(|filter| filter_matches(filter, event, budget))
}

/// Whether one filter matches `event`.
///
/// A pattern this matcher cannot read selects nothing: a stored pattern was held
/// to [`parse_filter_pattern`] when it was written, so reaching here with an
/// unreadable one would mean the event is not the subscriber's.
fn filter_matches(
    filter: &Filter,
    event: &DeliveredEvent,
    budget: &mut ::service_common::event_pattern::MatchBudget,
) -> bool {
    let Some(scoped) = event.scoped(&filter.scope) else {
        return false;
    };
    let Ok(pattern) = parse_filter_pattern(&filter.pattern) else {
        return false;
    };
    ::service_common::event_pattern::matches_within(&pattern, scoped, budget)
}

/// Read a filter's `Pattern` as an event pattern, or report why it is not one.
///
/// `FilterConfiguration$Language` declares one language,
/// `EVENT_BRIDGE_PATTERN`, so a filter's pattern is an EventBridge event
/// pattern and is read by the one shared reader of those. The `Err` is the
/// reason, for a handler to word into its own modelled refusal.
pub fn parse_filter_pattern(pattern: &str) -> Result<::serde_json::Value, String> {
    ::service_common::event_pattern::parse(pattern)
}

/// The member a filter configuration that selects nothing is missing, as a
/// refusal names it.
pub const FILTERS_MEMBER: &str = "Filters";

/// Why `configuration` is not a filter configuration a subscriber can carry, or
/// `None` when it is one.
///
/// `FilterConfiguration` states the contract: "On CreateSubscriber, Filters is
/// required and must contain at least one Filter with a non-empty Pattern …
/// Any non-empty shape (including `{Language:X}` without Filters) must contain a
/// valid Filters list — same contract as CreateSubscriber". So a configuration
/// that names no `Filters` at all selects nothing for the lifetime of the
/// subscriber and is a request parameter that is not valid, rather than a
/// subscriber that silently takes every event. A pattern that is not one this
/// service can evaluate is the same fault for the same reason.
///
/// The `Some` is the whole `Message` of the refusal, which is why it names the
/// member the request got wrong.
pub fn filter_configuration_fault(configuration: &FilterConfiguration) -> Option<String> {
    let filters = configuration.filters.as_deref().unwrap_or_default();
    if filters.is_empty() {
        return Some(format!("{FILTERS_MEMBER} is required"));
    }
    filters.iter().find_map(|filter| {
        parse_filter_pattern(&filter.pattern)
            .err()
            .map(|reason| format!("FilterConfiguration.{FILTERS_MEMBER}: {reason}"))
    })
}

/// Whether `configuration` is the EMPTY shape, which an update carries to CLEAR
/// the filter a subscriber holds.
///
/// `FilterConfiguration` separates the two: "On UpdateSubscriber, an empty
/// `FilterConfiguration:{}` clears the existing filter. Any non-empty shape
/// (including `{Language:X}` without Filters) must contain a valid Filters list".
/// So emptiness is a property of the SHAPE — no member of it carried at all —
/// rather than of the filters alone, and a configuration naming only a language
/// is non-empty and held to [`filter_configuration_fault`].
pub fn filter_configuration_clears(configuration: &FilterConfiguration) -> bool {
    configuration.language.is_none() && configuration.filters.is_none()
}

/// Why a subscriber cannot carry both this target configuration and this
/// transformer, or `None` when it can.
///
/// `Transformer` states the rule: it is "Not applicable to universal (aws-sdk)
/// targets, whose input transformation is UniversalTargetParameters.Input; a
/// Transformer on such a target is rejected". A universal target is the one
/// whose `UniversalTargetParameters` the subscriber's `InvokeConfiguration`
/// carries, so the two members are what the check reads.
///
/// The `Some` is the whole `Message` of the refusal, which is why it names the
/// members the request got wrong.
pub fn transformer_fault(
    universal_target_parameters: Option<&UniversalTargetParameters>,
    transformer: Option<&Transformer>,
) -> Option<String> {
    let (Some(_), Some(_)) = (universal_target_parameters, transformer) else {
        return None;
    };
    Some(
        "Transformer: a universal target takes its input transformation from \
         UniversalTargetParameters.Input and accepts no Transformer"
            .to_string(),
    )
}

/// What this subscriber's target is handed for `event`, or `None` when the
/// subscriber's `Transformer` names a transformation this slice does not perform
/// — in which case nothing is delivered rather than a document the transformer
/// did not produce.
///
/// `Transformer$Type` names the shape of that text. `RAW` — "Deliver the raw
/// event payload" — hands the target the event UNTRANSFORMED
/// ([`DeliveredEvent::untransformed`]): the envelope this service writes around
/// a structured entry's payload, and a raw entry's pre-shaped payload as its
/// publisher sent it. That is also what a subscriber configuring no transformer
/// hands its target, since a subscriber that asked for no transformation gets
/// none. `WITH_METADATA` hands it the event inside this service's own metadata
/// envelope ([`DeliveredEvent::document`]), which carries the event's position
/// on its bus's log and its system metadata beside the parts the event already
/// names. `JSONATA` names a transformation computed by an expression; this slice
/// performs none, so a subscriber configuring it delivers nothing here.
pub fn delivered_body(subscriber: &Subscriber, event: &DeliveredEvent) -> Option<String> {
    let transformer_type = subscriber
        .transformer
        .as_ref()
        .and_then(|transformer| transformer.r#type.as_deref());
    match transformer_type {
        None | Some(TRANSFORMER_TYPE_RAW) => Some(event.untransformed.clone()),
        Some(TRANSFORMER_TYPE_WITH_METADATA) => Some(event.document().to_string()),
        Some(_) => None,
    }
}

/// `at` as the instant a delivered event's metadata carries.
///
/// An event's instant is RFC 3339 at whole-second precision closed by `Z`
/// (`2026-01-01T00:00:00Z`), the form an event carries it in, which is the
/// resolution [`super::to_date_time`] already reports every instant of this
/// service at.
fn to_event_time(at: SystemTime) -> String {
    super::to_date_time(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// One subscriber's due deliveries: the subscriber, and the events of its bus's
/// log it is owed and has not been delivered yet.
#[derive(Debug, Clone)]
pub struct DueDelivery {
    pub subscriber: Subscriber,
    pub events: Vec<StoredEvent>,
}

/// The `ResourceNotFoundException` every subscriber-addressed operation answers
/// with when the store holds no subscriber at the requested ARN.
///
/// ONE construction, so no two operations word the same refusal differently: a
/// fixed sentence plus a single variable part, the ARN the request addressed.
///
/// The sentence names the KIND of resource that was not found followed by
/// `" not found: "` and the identifier the request supplied, the same way every
/// other resource of this service words that refusal
/// ([`super::event_bus_not_found`], [`super::event_source_not_found`]).
pub fn subscriber_not_found(subscriber_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("Subscriber not found: {subscriber_arn}")),
    }
}

impl ComAmazonawsEventbridgev2Awseventsv2State {
    /// The subscriber with this ARN, or `None` when no such subscriber exists.
    pub fn subscriber(&self, arn: &str) -> Option<&Subscriber> {
        self.subscribers.get(arn)
    }

    /// The subscriber carrying this name in this region.
    ///
    /// A `SubscriberArn` names a subscriber by its name and a minted id alone —
    /// the bus is not part of it — so a subscriber name is unique per account
    /// and region, the way a bus name is. A scan is enough at an emulator's
    /// scale and cannot drift out of step with [`Self::subscribers`] the way a
    /// second index could.
    pub fn subscriber_by_name(&self, region: &str, name: &str) -> Option<&Subscriber> {
        self.subscribers
            .values()
            .find(|subscriber| subscriber.region == region && subscriber.name == name)
    }

    /// The subscriber a create carrying this client token in this region
    /// minted, or `None` when no such subscriber is stored.
    ///
    /// The record is the SUBSCRIBER: a client token is read off the subscriber
    /// that carried it, so the replay index is the registry itself — nothing
    /// beside it can grow, and no record can outlive the resource it replays.
    pub fn subscriber_by_client_token(
        &self,
        region: &str,
        client_token: &str,
    ) -> Option<&Subscriber> {
        self.subscribers.values().find(|subscriber| {
            subscriber.region == region
                && subscriber.client_token.as_deref() == Some(client_token)
        })
    }

    /// Place the subscriber `arn` at the position an UNPAUSE resumes delivery
    /// from, as `ResumePosition` names it.
    ///
    /// `ResumePosition` is "honored only on an UpdateSubscriber call that
    /// transitions State from STOPPED to RUNNING (unpause)", so this is called
    /// only for that transition. `LAST_PROCESSED` — the default — "continue[s]
    /// delivery from where it stopped", which is the position the subscriber
    /// already holds, so nothing moves; `LATEST` "skip[s] the backlog accrued
    /// while stopped" by moving the position to the END of the bus's log, so the
    /// events published while the subscriber was stopped are never owed to it.
    ///
    /// A value `ResumePosition` does not declare is refused at the boundary
    /// before any handler runs, so it cannot reach here; one that does is
    /// nonetheless treated as the default rather than as a jump nobody asked
    /// for.
    pub fn resume_subscriber(&mut self, arn: &str, resume_position: Option<&str>) {
        if resume_position != Some(RESUME_POSITION_LATEST) {
            return;
        }
        let Some(subscriber) = self.subscribers.get(arn) else {
            return;
        };
        let Some(tip) = self
            .event_buses
            .get(&subscriber.event_bus_arn)
            .map(|bus| bus.next_sequence_number)
        else {
            return;
        };
        if let Some(subscriber) = self.subscribers.get_mut(arn) {
            subscriber.next_sequence_number = tip;
        }
    }

    /// Take what the subscriber `arn` is owed right now, advancing its position
    /// past the events handed back.
    ///
    /// MUTATING, and called only where the spec says this call makes deliveries
    /// due — a publish onto the bus, and the create that puts a subscriber at a
    /// position with a backlog behind it. The position advances HERE rather than
    /// at the target, so one event is handed to one subscriber once however many
    /// times either is read afterwards.
    ///
    /// Nothing is owed to a subscriber that is not delivering — "Delivery
    /// requires State RUNNING on a subscriber that is not revoked" — and such a
    /// subscriber's position stands still, which is what leaves it the backlog
    /// `ResumePosition` resumes from. The events past the subscriber's
    /// `EndPoint` are not owed either, and the position moves past them: that
    /// bound is an instant, so an event received after it is never delivered.
    pub fn take_due_delivery(&mut self, arn: &str) -> Option<DueDelivery> {
        let subscriber = self.subscribers.get(arn)?;
        if !subscriber.delivers() {
            return None;
        }
        let bus = self.event_buses.get(&subscriber.event_bus_arn)?;
        let end_point = subscriber.end_point();
        let from = subscriber.next_sequence_number;
        let events: Vec<StoredEvent> = bus
            .events
            .iter()
            .filter(|stored| stored.sequence_number >= from)
            .filter(|stored| end_point.is_none_or(|end| stored.received_at <= end))
            .cloned()
            .collect();
        let tip = bus.next_sequence_number;
        let subscriber = self.subscribers.get_mut(arn)?;
        subscriber.next_sequence_number = tip;
        Some(DueDelivery {
            subscriber: subscriber.clone(),
            events,
        })
    }

    /// Take what every subscriber of this bus is owed, as
    /// [`Self::take_due_delivery`] does for one.
    pub fn take_due_deliveries(&mut self, event_bus_arn: &str) -> Vec<DueDelivery> {
        let on_this_bus: Vec<String> = self
            .subscribers
            .values()
            .filter(|subscriber| subscriber.event_bus_arn == event_bus_arn)
            .map(|subscriber| subscriber.arn.clone())
            .collect();
        on_this_bus
            .iter()
            .filter_map(|arn| self.take_due_delivery(arn))
            .filter(|due| !due.events.is_empty())
            .collect()
    }

    /// How many subscribers of this bus have NOT been revoked.
    ///
    /// A subscriber is created on one bus and keeps delivering that bus's
    /// events until it is deleted or revoked ([`Subscriber::delivers`]), so the
    /// unrevoked subscribers are the ones still using their bus — which is what
    /// makes the bus in use for `DeleteEventBus`. A revoked subscriber stays
    /// readable but delivers nothing, so it holds nothing: revoking is one of
    /// the two remedies that refusal names, beside deleting
    /// (`crate::ops::delete_event_bus::SUBSCRIBERS_IN_USE_MESSAGE`). A STOPPED
    /// subscriber is not one of them — `State` is caller-controlled and
    /// reversible, so such a subscriber is still the bus's.
    pub fn unrevoked_subscribers_of_bus(&self, event_bus_arn: &str) -> usize {
        self.subscribers
            .values()
            .filter(|subscriber| subscriber.event_bus_arn == event_bus_arn && !subscriber.revoked)
            .count()
    }

    /// Remove the subscriber at this ARN, reporting whether one was there.
    ///
    /// The subscriber IS the record of its own client token, so forgetting it
    /// frees both its name and that token: a later create may take the name
    /// again, and a retry carrying the token mints a new subscriber instead of
    /// replaying a deleted one.
    pub fn remove_subscriber(&mut self, arn: &str) -> bool {
        self.subscribers.remove(arn).is_some()
    }

    /// Forget the subscribers that were still DELIVERING a bus which has
    /// finished disappearing.
    ///
    /// A subscriber that still delivers its bus's events cannot outlive it: one
    /// left behind would be delivering from a bus no read can resolve. Its
    /// client token goes with it, because the token is held by the subscriber
    /// itself.
    ///
    /// A REVOKED subscriber is not one of those. Revoking it is what released
    /// the bus for deletion ([`Self::unrevoked_subscribers_of_bus`]) and
    /// revocation is terminal with the delete left available for cleanup, so
    /// the record outlives the bus — readable, reporting `Revoked` and the
    /// `EventBusArn` it was created on — until its own delete removes it.
    pub fn forget_subscribers_of_bus(&mut self, event_bus_arn: &str) {
        let orphaned: Vec<String> = self
            .subscribers
            .values()
            .filter(|subscriber| subscriber.event_bus_arn == event_bus_arn && !subscriber.revoked)
            .map(|subscriber| subscriber.arn.clone())
            .collect();
        for arn in orphaned {
            self.subscribers.remove(&arn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FILTER_SCOPE_DATA, FILTER_SCOPE_METADATA, FILTER_SCOPE_SYSTEM_METADATA,
        ORDERING_TYPE_UNORDERED, RESUME_POSITION_LAST_PROCESSED, RESUME_POSITION_LATEST,
        STARTING_POSITION_LATEST, SUBSCRIBER_STATE_RUNNING, SUBSCRIBER_STATE_STOPPED,
        TRANSFORMER_TYPE_RAW, TRANSFORMER_TYPE_WITH_METADATA,
    };
    use crate::ops::types::{
        FILTER_SCOPE_VALUES, ORDERING_TYPE_VALUES, RESUME_POSITION_VALUES,
        STARTING_POSITION_VALUES, SUBSCRIBER_STATE_VALUES, TRANSFORMER_TYPE_VALUES,
    };

    /// Every value this slice puts on the wire, or keys a decision on, is a
    /// value the model declares, so these constants cannot drift from the
    /// generated enums.
    #[test]
    fn the_subscriber_vocabulary_is_the_modelled_one() {
        for (case, value, declared) in [
            (
                "SubscriberState::RUNNING",
                SUBSCRIBER_STATE_RUNNING,
                SUBSCRIBER_STATE_VALUES,
            ),
            (
                "SubscriberState::STOPPED",
                SUBSCRIBER_STATE_STOPPED,
                SUBSCRIBER_STATE_VALUES,
            ),
            (
                "OrderingType::UNORDERED",
                ORDERING_TYPE_UNORDERED,
                ORDERING_TYPE_VALUES,
            ),
            (
                "StartingPosition::LATEST",
                STARTING_POSITION_LATEST,
                STARTING_POSITION_VALUES,
            ),
            (
                "ResumePosition::LAST_PROCESSED",
                RESUME_POSITION_LAST_PROCESSED,
                RESUME_POSITION_VALUES,
            ),
            (
                "ResumePosition::LATEST",
                RESUME_POSITION_LATEST,
                RESUME_POSITION_VALUES,
            ),
            ("FilterScope::DATA", FILTER_SCOPE_DATA, FILTER_SCOPE_VALUES),
            (
                "FilterScope::METADATA",
                FILTER_SCOPE_METADATA,
                FILTER_SCOPE_VALUES,
            ),
            (
                "FilterScope::SYSTEM_METADATA",
                FILTER_SCOPE_SYSTEM_METADATA,
                FILTER_SCOPE_VALUES,
            ),
            (
                "TransformerType::RAW",
                TRANSFORMER_TYPE_RAW,
                TRANSFORMER_TYPE_VALUES,
            ),
            (
                "TransformerType::WITH_METADATA",
                TRANSFORMER_TYPE_WITH_METADATA,
                TRANSFORMER_TYPE_VALUES,
            ),
        ] {
            assert!(
                declared.contains(&value),
                "case {case:?}: {value:?} is not one of the modelled values {declared:?}"
            );
        }
    }
}
