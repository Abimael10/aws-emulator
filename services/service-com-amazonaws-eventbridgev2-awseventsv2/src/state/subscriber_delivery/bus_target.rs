// SPDX-License-Identifier: CC0-1.0

//! The publish a subscriber whose target is ANOTHER EVENT BUS delivers with:
//! each accepted event is forwarded onto that bus as a new event of it.
//!
//! `EventBusV2Parameters` is what such a subscriber is configured with —
//! "parameters for forwarding events to another EventBridge event bus" — and
//! every value it carries is "a literal or a JSONata expression", so each is
//! resolved against the event being forwarded
//! ([`super::super::event_expressions`]) before the forward is made: the
//! `Metadata` "forwarded with each event", the `SystemMetadata` "attached to
//! each forwarded event" and the `DeduplicationConfiguration` "applied to the
//! forwarded events on the downstream bus".
//!
//! The downstream bus is a resource of THIS service, so the forward is an append
//! to this same store ([`super::super::ComAmazonawsEventbridgev2Awseventsv2State::forward_events`])
//! rather than a call over a client: no socket is opened and the forwarded event
//! is an event of that bus in its own right, with that bus's sequence numbers,
//! retention and deduplication index. What the subscribers of that bus are owed
//! by it is reported back to [`super::deliver_due`], which delivers it — so a
//! fan-out chain is walked within the publish that set it off, up to
//! [`super::MAX_FORWARD_HOPS`] hops.
//!
//! The sibling of the queue and topic calls in [`super`], and held to the same
//! two rules: the role decides the publish before it is made
//! ([`super::delivery_authorization`]) and an event the downstream bus did not
//! take is recorded as a failed delivery ([`super::dead_letter`]).

use std::collections::BTreeMap;

use super::dead_letter::Failure;
use super::{Delivery, Subscriber};
use crate::state::{DueDelivery, ForwardedEvent};

/// The `ContentType` a forwarded event whose bytes are one JSON document
/// carries.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The `ContentType` a forwarded event whose bytes are no document carries:
/// what the forward publishes is the text the subscriber's transformer produced.
const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// What one forward came to: the deliveries the downstream bus did not take,
/// and what its own subscribers are owed by the ones it did.
pub(super) struct Forwarded<'a> {
    pub(super) undelivered: Vec<(&'a Delivery, Failure)>,
    pub(super) made_due: Vec<DueDelivery>,
}

/// The event one delivery is FORWARDED as, with every value the subscriber's
/// `EventBusV2Parameters` contributes resolved against the event it carries.
pub(super) fn forwarded(
    subscriber: &Subscriber,
    delivery: &Delivery,
    budget: &mut crate::state::EvaluationBudget,
) -> ForwardedEvent {
    let parameters = subscriber
        .invoke_configuration
        .event_bus_v2_parameters
        .as_ref();
    let content_based = parameters
        .and_then(|parameters| parameters.deduplication_configuration.as_ref())
        .is_some_and(|configuration| {
            configuration.deduplication_type == crate::state::CONTENT_BASED_DEDUPLICATION
        });
    ForwardedEvent {
        body: delivery.body.clone(),
        content_type: content_type_of(delivery),
        metadata: metadata_of(subscriber, delivery, budget),
        event_group_id: super::configured(
            subscriber,
            delivery,
            parameters
                .and_then(|parameters| parameters.system_metadata.as_ref())
                .and_then(|metadata| metadata.event_group_id.as_deref()),
            budget,
        ),
        deduplication_id: super::configured(
            subscriber,
            delivery,
            parameters
                .and_then(|parameters| parameters.system_metadata.as_ref())
                .and_then(|metadata| metadata.deduplication_id.as_deref()),
            budget,
        ),
        content_based_deduplication: content_based,
    }
}

/// Forward the prepared events onto the event bus `bus_arn` as new events of
/// it.
pub(super) fn forward<'a>(
    store: &::engine::ServiceState<crate::state::ComAmazonawsEventbridgev2Awseventsv2State>,
    subscriber: &Subscriber,
    bus_arn: &str,
    prepared: &'a [(Delivery, ForwardedEvent)],
    hop: usize,
) -> Forwarded<'a> {
    // A forward makes the downstream bus's own subscribers owed the event, and
    // one of those may forward again: the chain is bounded, and a forward past
    // the bound is not made at all rather than reported as made.
    if hop >= super::MAX_FORWARD_HOPS {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            target = %bus_arn,
            "an EventBridge subscriber's forward was not made: it is more than {} forwards from \
             the publish that set it off",
            super::MAX_FORWARD_HOPS
        );
        return Forwarded {
            undelivered: Vec::new(),
            made_due: Vec::new(),
        };
    }
    let events: Vec<ForwardedEvent> = prepared
        .iter()
        .map(|(_, forwarded)| forwarded.clone())
        .collect();
    // The store's lock is taken for the append alone and released with this
    // statement: the deliveries it makes due are made by the caller.
    let forwarded = store.lock().forward_events(
        bus_arn,
        &events,
        // The forwarded event is received by the downstream bus now: the
        // forward is part of the publish that made it due.
        ::std::time::SystemTime::now(),
    );
    match forwarded {
        Ok(made_due) => Forwarded {
            undelivered: Vec::new(),
            made_due,
        },
        Err(fault) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                target = %bus_arn,
                "an EventBridge subscriber's forward was not made: {}",
                fault.reason()
            );
            // A bus that is not there is the resource the forward could not
            // reach; one that is there and will not take events answered
            // about its own state, which the record carries as that answer.
            let failure = match &fault {
                crate::state::ForwardFault::NoSuchBus(_) => Failure::absent(bus_arn),
                crate::state::ForwardFault::NotAccepting(reason) => {
                    Failure::refused(reason.clone())
                }
            };
            Forwarded {
                undelivered: prepared
                    .iter()
                    .map(|(delivery, _)| (delivery, failure.clone()))
                    .collect(),
                made_due: Vec::new(),
            }
        }
    }
}

/// What the bytes this forward publishes ARE, as the forwarded event's own
/// `ContentType`: a JSON document when the body is one — which is what a
/// subscriber of the downstream bus then reads its `DATA` out of — and the text
/// it is otherwise.
fn content_type_of(delivery: &Delivery) -> String {
    if crate::state::json_document(delivery.body.as_bytes()).is_some() {
        JSON_CONTENT_TYPE.to_string()
    } else {
        TEXT_CONTENT_TYPE.to_string()
    }
}

/// The `Metadata` the forwarded event carries: each pair of the subscriber's
/// own, resolved against the event being forwarded.
///
/// "Keys and values are unconstrained strings so either side can contain a
/// JSONata expression" (`EventBusV2MetadataMap`), so both sides are resolved; a
/// pair whose key or value produced nothing is not forwarded, because a
/// metadata member the subscriber meant to compute is not one with a literal
/// expression for its value.
fn metadata_of(
    subscriber: &Subscriber,
    delivery: &Delivery,
    budget: &mut crate::state::EvaluationBudget,
) -> BTreeMap<String, String> {
    let configured = subscriber
        .invoke_configuration
        .event_bus_v2_parameters
        .as_ref()
        .and_then(|parameters| parameters.metadata.as_ref());
    let mut metadata = BTreeMap::new();
    for (name, value) in configured.into_iter().flatten() {
        let (Some(name), Some(value)) = (
            super::configured(subscriber, delivery, Some(name.as_str()), budget),
            super::configured(subscriber, delivery, Some(value.as_str()), budget),
        ) else {
            continue;
        };
        metadata.insert(name, value);
    }
    metadata
}
