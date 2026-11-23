// SPDX-License-Identifier: CC0-1.0

//! What a subscriber whose target is ANOTHER event bus does with a matched
//! event: it publishes it onto that bus as a new event.
//!
//! `EventBusV2Parameters` is what such a subscriber is configured with —
//! "parameters for forwarding events to another EventBridge event bus", with the
//! `Metadata` "forwarded with each event", the `SystemMetadata` "attached to
//! each forwarded event" and a `DeduplicationConfiguration` "applied to the
//! forwarded events on the downstream bus". So the forwarded event is an event
//! of the downstream bus in its own right: it takes that bus's sequence numbers,
//! its retention and its deduplication index, and the subscribers of THAT bus
//! are owed it exactly as they are owed anything else published there.
//!
//! The forward is made IN THIS STORE rather than over a client, because the
//! downstream bus is a resource of this same service: one store holds both
//! buses, so the publish is the same append [`super::ComAmazonawsEventbridgev2Awseventsv2State`]
//! performs for `PutRawEvents` — the forwarded event carries DATA, a content
//! type naming its form and the publisher's metadata, which is exactly the shape
//! a raw entry publishes.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{
    token_deduplication_key, ComAmazonawsEventbridgev2Awseventsv2State, ContentDigest, DueDelivery,
    EventPayload, RawPayload, StoredEvent,
};

/// The `DeduplicationType` the model declares, per
/// `smithy_query(query="shape",shape="DeduplicationType")`: the one mode that
/// keys on the event's content rather than on a token the publisher states.
pub const CONTENT_BASED_DEDUPLICATION: &str = "CONTENT_BASED";

/// One event a subscriber forwards onto another event bus, with every value its
/// `EventBusV2Parameters` contributes already resolved against the event it
/// carries ([`super::event_expressions`]).
#[derive(Debug, Clone)]
pub struct ForwardedEvent {
    /// The event as the subscriber's `Transformer` shapes it, which is the DATA
    /// the forwarded event carries.
    pub body: String,
    /// What those bytes are, as the forwarded event's own `ContentType`.
    pub content_type: String,
    /// The `Metadata` the subscriber forwards with each event, in key order
    /// because a map carries no order on the wire.
    pub metadata: BTreeMap<String, String>,
    /// The `SystemMetadata.EventGroupId` the forwarded event carries, for
    /// ordering on the downstream bus.
    pub event_group_id: Option<String>,
    /// The `SystemMetadata.DeduplicationId` it carries.
    pub deduplication_id: Option<String>,
    /// Whether the subscriber's `DeduplicationConfiguration` asks the downstream
    /// bus to deduplicate by the event's CONTENT instead of by that token.
    pub content_based_deduplication: bool,
}

impl ForwardedEvent {
    /// The key the downstream bus indexes this event under, or `None` when the
    /// subscriber asked for no deduplication at all.
    ///
    /// The same two keys a publish is deduplicated by
    /// ([`super::token_deduplication_key`], [`super::ContentDigest`]): the
    /// content key covers the data, the content type that says what those bytes
    /// are and the metadata the forward carries, which is the event the bus
    /// stores and delivers.
    fn deduplication_key(&self) -> Option<String> {
        if !self.content_based_deduplication {
            return self.deduplication_id.as_deref().map(token_deduplication_key);
        }
        let mut digest = ContentDigest::new();
        digest.absorb_text(Some(self.content_type.as_str()));
        digest.absorb_bytes(Some(self.body.as_bytes()));
        digest.absorb_count(self.metadata.len());
        for (name, value) in &self.metadata {
            digest.absorb_text(Some(name.as_str()));
            digest.absorb_text(Some(value.as_str()));
        }
        Some(digest.key())
    }
}

/// Why a forward onto the downstream bus was not made.
#[derive(Debug, Clone)]
pub enum ForwardFault {
    /// No bus of this store answers to that `EventBusArn`.
    NoSuchBus(String),
    /// The bus is there but its state does not accept events.
    NotAccepting(String),
}

impl ForwardFault {
    /// What a log and a dead-letter record say about the fault.
    pub fn reason(&self) -> &str {
        match self {
            Self::NoSuchBus(reason) | Self::NotAccepting(reason) => reason,
        }
    }
}

impl ComAmazonawsEventbridgev2Awseventsv2State {
    /// Publish `forwarded` onto the bus `event_bus_arn` and report what the
    /// subscribers of THAT bus are owed now that its log has grown.
    ///
    /// The `Err` is why the bus took nothing: a bus no read of this store
    /// resolves, or one whose state does not accept events — "an event bus that
    /// is still being created cannot accept events". Either way nothing is
    /// appended, so a forward the downstream bus could not take is recorded as a
    /// delivery that did not happen rather than reported as one that did.
    pub fn forward_events(
        &mut self,
        event_bus_arn: &str,
        forwarded: &[ForwardedEvent],
        now: SystemTime,
    ) -> Result<Vec<DueDelivery>, ForwardFault> {
        self.settle_lifecycles(now);
        let Some(bus) = self.bus(event_bus_arn) else {
            return Err(ForwardFault::NoSuchBus(format!(
                "no event bus {event_bus_arn} exists"
            )));
        };
        if !bus.accepts_events() {
            return Err(ForwardFault::NotAccepting(format!(
                "event bus {event_bus_arn} is in state {} and cannot accept events",
                bus.state.wire_value()
            )));
        }
        // One identifier seed per forwarded event, reserved from the STORE's own
        // counter, so a forwarded event's `EventId` is unique across every bus
        // this emulator holds.
        let id_seeds: Vec<u64> = (0..forwarded.len()).map(|_| self.next_id_seed()).collect();
        // Read through the registry again rather than through the reference
        // above, because the events are appended to the bus: nothing has
        // touched the store since, so a bus that has gone missing here is
        // reported as the bus that is not there rather than unwinding.
        let Some(bus) = self.event_buses.get_mut(event_bus_arn) else {
            return Err(ForwardFault::NoSuchBus(format!(
                "no event bus {event_bus_arn} exists"
            )));
        };
        for (event, id_seed) in forwarded.iter().zip(id_seeds) {
            let deduplication_key = event.deduplication_key();
            let already_published = deduplication_key
                .as_deref()
                .is_some_and(|key| bus.deduplicated_events.contains_key(key));
            if already_published {
                continue;
            }
            let stored = StoredEvent {
                event_id: super::new_event_id(id_seed, now),
                sequence_number: bus.next_sequence_number,
                payload: EventPayload::Raw(RawPayload {
                    data: event.body.clone().into_bytes(),
                    content_type: event.content_type.clone(),
                    metadata: event.metadata.clone(),
                }),
                // A forwarded event is a new event of the downstream bus, so it
                // occurred when that bus received it.
                time: now,
                received_at: now,
                deduplication_id: event.deduplication_id.clone(),
                deduplication_key: deduplication_key.clone(),
                event_group_id: event.event_group_id.clone(),
            };
            bus.next_sequence_number = bus.next_sequence_number.saturating_add(1);
            if let Some(key) = deduplication_key {
                bus.deduplicated_events
                    .insert(key, (stored.event_id.clone(), stored.sequence_number));
            }
            bus.events.push(stored);
        }
        // The emulator's own memory bound on the stored log, and on the
        // deduplication index with it.
        bus.trim_to_capacity();
        Ok(self.take_due_deliveries(event_bus_arn))
    }
}
