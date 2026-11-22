// SPDX-License-Identifier: CC0-1.0

//! The EVENT HISTORY of a Spot Fleet request: the events the fleet itself has been through,
//! the words the model publishes for each one, and where each sits in the order a report of
//! them pages over.
//!
//! "Describes an event in the history of the Spot Fleet request" (`HistoryRecord`): an event
//! is a FACT of the fleet, recorded at the instant it happened by whichever writer of
//! [`crate::state`] made it happen — the write that stored the fleet, the clock-owed move that
//! changed its `BatchState`, the fulfillment that launched one of its instances and the
//! termination that ended one. Nothing derives an event from a read, so two reports of one
//! fleet's history a moment apart carry the same records.
//!
//! The history of one fleet is bounded by the fleet itself: one event for each `BatchState` it
//! ENTERS (at most [`crate::ops::types::BATCH_STATE_VALUES`] of them — a call that asks a fleet
//! for the state it is already in changes nothing and so records nothing, which is what keeps
//! this a bound on the fleet rather than on how often a caller calls) and at most two — a
//! launch and a termination — for each Spot Instance request it placed, which
//! [`MAX_SPOT_FLEET_SPOT_INSTANCES`] bounds. It holds no caller-chosen text at all: every
//! word is a `&'static str` of a modelled catalog and every instance id was minted by this
//! store, so it adds nothing to the store's caller-chosen byte charge
//! ([`spot_fleet_request_payload_bytes`]).
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// The `fleetRequestChange` member of the modelled `EventType` catalog
/// ([`crate::ops::types::EVENT_TYPE_VALUES`]): "a change in the status or configuration of the
/// Spot Fleet request" (`HistoryRecord$EventType`).
pub const EVENT_TYPE_FLEET_REQUEST_CHANGE: &str = "fleetRequestChange";

/// The `instanceChange` member of [`crate::ops::types::EVENT_TYPE_VALUES`]: "an instance was
/// launched or terminated" (`HistoryRecord$EventType`).
pub const EVENT_TYPE_INSTANCE_CHANGE: &str = "instanceChange";

/// The `launched` `instanceChange` sub type: "`launched` - a new instance was launched"
/// (`EventInformation$EventSubType`).
pub const EVENT_SUB_TYPE_LAUNCHED: &str = "launched";

/// The `terminated` `instanceChange` sub type: "`terminated` - an instance was terminated by
/// the user" (`EventInformation$EventSubType`).
pub const EVENT_SUB_TYPE_TERMINATED: &str = "terminated";

/// How far back a report of a Spot Fleet request's history reaches: "Spot Fleet events are
/// available for 48 hours" (`DescribeSpotFleetRequestHistory`), so an event older than that is
/// no longer one a report can carry, however early a window a request asks for.
pub const SPOT_FLEET_HISTORY_AVAILABILITY: Duration = Duration::from_secs(48 * 60 * 60);

/// One event in the history of one Spot Fleet request.
///
/// The modelled [`HistoryRecord`](crate::ops::types::HistoryRecord) is PROJECTED from this
/// ([`spot_fleet_history_record_view`]) rather than stored, because the two words an event
/// carries are members of modelled catalogs — held as `&'static str` so only a word from a
/// catalog constant of this module can ever be recorded, never a request's text.
#[derive(Debug, Clone)]
pub struct SpotFleetHistoryEvent {
    /// When the event happened: "the date and time of the event, in UTC format"
    /// (`HistoryRecord$Timestamp`), which is the instant the change itself fell due and not
    /// the instant anybody read it.
    pub at: SystemTime,
    /// "The event type" (`HistoryRecord$EventType`), a member of
    /// [`crate::ops::types::EVENT_TYPE_VALUES`].
    pub event_type: &'static str,
    /// "The event" (`EventInformation$EventSubType`) — which event of that type this is.
    pub event_sub_type: &'static str,
    /// "The ID of the instance. This information is available only for `instanceChange`
    /// events" (`EventInformation$InstanceId`), so a `fleetRequestChange` event carries none.
    pub instance_id: Option<String>,
}

impl SpotFleetHistoryEvent {
    /// The event a Spot Fleet request's `BatchState` change is.
    ///
    /// "A change in the status or configuration of the Spot Fleet request"
    /// ([`EVENT_TYPE_FLEET_REQUEST_CHANGE`]), whose sub type is the `BatchState` member the
    /// fleet ENTERED: the sub types `EventInformation$EventSubType` documents for a
    /// `fleetRequestChange` event — `submitted`, `active`, `cancelled`, `cancelled_running`,
    /// `cancelled_terminating`, `modify_in_progress`, `modify_succeeded`, `expired` — name
    /// the state the request reached, so the word is taken from the state catalog itself
    /// ([`SPOT_FLEET_STATE_ACTIVE`] and its neighbours) rather than from a second list that
    /// could drift from it.
    pub fn fleet_request_change(state: &'static str, at: SystemTime) -> Self {
        Self {
            at,
            event_type: EVENT_TYPE_FLEET_REQUEST_CHANGE,
            event_sub_type: state,
            instance_id: None,
        }
    }

    /// The event one of the fleet's instances being launched or terminated is
    /// ([`EVENT_TYPE_INSTANCE_CHANGE`]), naming that instance: "this information is available
    /// only for `instanceChange` events" (`EventInformation$InstanceId`).
    pub fn instance_change(
        event_sub_type: &'static str,
        instance_id: &str,
        at: SystemTime,
    ) -> Self {
        Self {
            at,
            event_type: EVENT_TYPE_INSTANCE_CHANGE,
            event_sub_type,
            instance_id: Some(instance_id.to_string()),
        }
    }
}

/// One stored event as the modelled `HistoryRecord` every report of a fleet's history answers
/// with.
///
/// `EventInformation$EventDescription` — "the description of the event" — is deliberately left
/// absent: the model makes it optional and this store holds no text about an event beyond the
/// two catalog words it is, so reporting none is the one honest answer. The `InstanceId` is
/// carried only by the events that have one, which is what the member documents.
pub fn spot_fleet_history_record_view(
    event: &SpotFleetHistoryEvent,
) -> crate::ops::types::HistoryRecord {
    crate::ops::types::HistoryRecord {
        event_information: Some(crate::ops::types::EventInformation {
            event_description: None,
            event_sub_type: Some(event.event_sub_type.to_string()),
            instance_id: event.instance_id.clone(),
        }),
        event_type: Some(event.event_type.to_string()),
        timestamp: Some(system_time_to_date_time(event.at)),
    }
}

/// Where one event sits in the order a report of a fleet's history pages over: the instant it
/// happened, then the position it was recorded at.
///
/// The instant alone is not a position — two events of one fleet can fall due in the same
/// instant — so the recorded position breaks the tie, which makes the order TOTAL and lets a
/// cursor always advance. Both halves are printed at a fixed width, so ordering these strings
/// orders the history the way it ran, oldest event first, and an event recorded later is
/// appended rather than shifting the position of one already reported.
pub fn spot_fleet_history_position(at: SystemTime, recorded: usize) -> String {
    let since_epoch = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let separator = CURSOR_SCOPE_SEPARATOR;
    format!(
        "{:020}.{:09}{separator}{recorded:020}",
        since_epoch.as_secs(),
        since_epoch.subsec_nanos()
    )
}

impl ComAmazonawsEc2Amazonec2State {
    /// Record one event in the history of the Spot Fleet request `fleet_id` names, if this
    /// store still holds that fleet.
    ///
    /// How a writer that reaches a fleet BY ID records an event, so every such event is
    /// appended the same way and in the order the events happened. A writer that already holds
    /// the record itself — [`SpotFleetRequestRecord::apply_due_move`], which is handed the
    /// record rather than the store — appends to [`SpotFleetRequestRecord::history`] directly;
    /// both append the same [`SpotFleetHistoryEvent`], which is where the shape of an event is
    /// decided.
    pub(super) fn record_spot_fleet_history_event(
        &mut self,
        fleet_id: &str,
        event: SpotFleetHistoryEvent,
    ) {
        if let Some(record) = self.spot_fleet_requests.get_mut(fleet_id) {
            record.history.push(event);
        }
    }

    /// Record the launch or termination of one instance in the history of the Spot Fleet
    /// request that PLACED it, if a fleet of this store placed it at all.
    ///
    /// An instance reaches a fleet's history through the Spot Instance request the fleet
    /// placed it with ([`SpotFleetPlacement`]), which is the one way a fleet's instances are
    /// reached anywhere in this store: an instance a caller launched itself belongs to no
    /// fleet and so records nothing.
    pub(super) fn record_spot_fleet_instance_event(
        &mut self,
        spot_instance_request_id: &str,
        instance_id: &str,
        event_sub_type: &'static str,
        at: SystemTime,
    ) {
        let placed_by = self
            .spot_fleet_requests
            .iter()
            .find(|(_, record)| {
                record
                    .placements
                    .iter()
                    .any(|placement| placement.spot_instance_request_id == spot_instance_request_id)
            })
            .map(|(fleet_id, _)| fleet_id.clone());
        if let Some(fleet_id) = placed_by {
            self.record_spot_fleet_history_event(
                &fleet_id,
                SpotFleetHistoryEvent::instance_change(event_sub_type, instance_id, at),
            );
        }
    }
}
