// SPDX-License-Identifier: CC0-1.0

//! The EVENT HISTORY of an EC2 Fleet: the words an event of one carries, how long a report of
//! them reaches back, where each event sits in the order a report pages over, and the writers
//! that record one.
//!
//! "Describes an event in the history of an EC2 Fleet" (`HistoryRecordEntry`): an event is a
//! FACT of the fleet, recorded at the instant it happened by whichever writer of
//! [`crate::state`] made it happen — the write that stored the fleet and placed its capacity,
//! the clock-owed move that changed its `FleetStateCode`, the deletion that asked for it, and
//! the termination of one of the instances it placed. Nothing derives an event from a read, so
//! two reports of one fleet's history a moment apart carry the same records.
//!
//! An event of an EC2 Fleet and an event of a Spot Fleet request are ONE shape — "Describes an
//! EC2 Fleet or Spot Fleet event" (`EventInformation`) — so the stored event is the one
//! [`SpotFleetHistoryEvent`] both histories hold ([`FleetHistoryEvent`]), the sub type words
//! they share are the ones that member publishes ([`EVENT_SUB_TYPE_LAUNCHED`] and
//! [`EVENT_SUB_TYPE_TERMINATED`]), and the order a report pages over is the one
//! [`fleet_history_position`] defines for both. What differs is the `EventType` catalog each
//! reports from — `FleetEventType` for an EC2 Fleet, `EventType` for a Spot Fleet request — so
//! the words below are this one's, and the projection answers this one's modelled shape.
//!
//! The history of one fleet is bounded by the fleet itself: one event for each
//! `FleetStateCode` it ENTERS (at most [`crate::ops::types::FLEET_STATE_CODE_VALUES`] of them)
//! and at most two — a launch and a termination — for each instance it placed, which
//! [`MAX_FLEET_INSTANCES`] bounds. It holds no caller-chosen text at all: every word is a
//! `&'static str` of a modelled catalog and every instance id was minted by this store and is
//! already charged by [`fleet_payload_bytes`], so it adds nothing to the store's
//! caller-chosen byte charge.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// The `fleet-change` member of the modelled `FleetEventType` catalog
/// ([`crate::ops::types::FLEET_EVENT_TYPE_VALUES`]): a change in the status or configuration
/// of the EC2 Fleet itself, whose sub type is the state the fleet reached.
pub const FLEET_EVENT_TYPE_FLEET_CHANGE: &str = "fleet-change";

/// The `instance-change` member of [`crate::ops::types::FLEET_EVENT_TYPE_VALUES`]: "`launched`
/// - a new instance was launched" or "`terminated` - an instance was terminated by the user"
/// (`EventInformation$EventSubType`).
pub const FLEET_EVENT_TYPE_INSTANCE_CHANGE: &str = "instance-change";

/// How far back a report of an EC2 Fleet's history reaches: "EC2 Fleet events are available
/// for 48 hours" (`DescribeFleetHistory`), so an event older than that is no longer one a
/// report can carry, however early a window a request asks for.
pub const FLEET_HISTORY_AVAILABILITY: Duration = Duration::from_secs(48 * 60 * 60);

/// How long after an event happened before a report of a fleet's history can carry it: "EC2
/// Fleet events are delayed by up to 30 seconds before they can be described"
/// (`DescribeFleetHistory`), which is the published span and so the one this store answers.
///
/// It is what makes the operation's own promise hold — "this ensures that you can query by the
/// last evaluated time and not miss a recorded event" — because a report carries every event up
/// to the instant it reports as evaluated ([`fleet_history_evaluated_until`]) and the events of
/// the span after it have not been published to any caller yet.
pub const FLEET_HISTORY_PUBLICATION_DELAY: Duration = Duration::from_secs(30);

/// The instant a report of a fleet's history is complete up to, as of `now`: an event recorded
/// at or before it is describable, and one recorded after it is not published yet
/// ([`FLEET_HISTORY_PUBLICATION_DELAY`]).
///
/// Both halves of the operation's contract read this one instant — it ends the window a report
/// selects over, and it is the `LastEvaluatedTime` the report answers with — so a caller that
/// asks again from the instant it was told about misses no event, which is what
/// "`LastEvaluatedTime` … all records up to this time were retrieved" promises.
pub fn fleet_history_evaluated_until(now: SystemTime) -> SystemTime {
    now.checked_sub(FLEET_HISTORY_PUBLICATION_DELAY)
        .unwrap_or(UNIX_EPOCH)
}

/// One event in the history of one EC2 Fleet.
///
/// The SAME stored event a Spot Fleet request's history holds, because the two are one modelled
/// shape: an instant, the event's type and sub type as words of a modelled catalog, and the
/// instance an instance event names. The modelled
/// [`HistoryRecordEntry`](crate::ops::types::HistoryRecordEntry) is PROJECTED from it
/// ([`fleet_history_record_view`]) rather than stored, so only a word from a catalog constant
/// of this crate can ever be recorded, never a request's text.
pub type FleetHistoryEvent = SpotFleetHistoryEvent;

/// The event one EC2 Fleet's `FleetStateCode` change is.
///
/// A change in the status of the fleet ([`FLEET_EVENT_TYPE_FLEET_CHANGE`]), whose sub type is
/// the `FleetStateCode` member the fleet ENTERED: the sub types
/// `EventInformation$EventSubType` documents for a fleet change — `submitted`, `active`,
/// `deleted`, `deleted_running`, `deleted_terminating`, `expired`, `modify_in_progress`,
/// `modify_succeeded` — name the state the fleet reached, so the word is taken from the state
/// catalog itself ([`FLEET_STATE_ACTIVE`] and its neighbours) rather than from a second list
/// that could drift from it.
pub fn fleet_change_event(state: &'static str, at: SystemTime) -> FleetHistoryEvent {
    FleetHistoryEvent {
        at,
        event_type: FLEET_EVENT_TYPE_FLEET_CHANGE,
        event_sub_type: state,
        // "This information is available only for `instanceChange` events"
        // (`EventInformation$InstanceId`), so a change of the fleet itself names no instance.
        instance_id: None,
    }
}

/// The event one of the fleet's instances being launched or terminated is
/// ([`FLEET_EVENT_TYPE_INSTANCE_CHANGE`]), naming that instance: "this information is
/// available only for `instanceChange` events" (`EventInformation$InstanceId`).
pub fn fleet_instance_change_event(
    event_sub_type: &'static str,
    instance_id: &str,
    at: SystemTime,
) -> FleetHistoryEvent {
    FleetHistoryEvent {
        at,
        event_type: FLEET_EVENT_TYPE_INSTANCE_CHANGE,
        event_sub_type,
        instance_id: Some(instance_id.to_string()),
    }
}

/// The `FleetStateCode` member `state` is, as a `&'static str` of the modelled catalog
/// ([`crate::ops::types::FLEET_STATE_CODE_VALUES`]) — `None` for anything that is not a member
/// of it.
///
/// How a writer holding a fleet's state as stored TEXT records the event that state is: the
/// word is resolved back to the catalog, so a history can only ever hold a modelled state word
/// and nothing a request supplied can reach it.
pub fn fleet_state_word(state: &str) -> Option<&'static str> {
    crate::ops::types::FLEET_STATE_CODE_VALUES
        .iter()
        .copied()
        .find(|member| *member == state)
}

/// One stored event as the modelled `HistoryRecordEntry` a report of an EC2 Fleet's history
/// answers with.
///
/// `EventInformation$EventDescription` — "the description of the event" — is deliberately left
/// absent: the model makes it optional and this store holds no text about an event beyond the
/// two catalog words it is, so reporting none is the one honest answer. The `InstanceId` is
/// carried only by the events that have one, which is what the member documents.
pub fn fleet_history_record_view(
    event: &FleetHistoryEvent,
) -> crate::ops::types::HistoryRecordEntry {
    crate::ops::types::HistoryRecordEntry {
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
///
/// The ONE ordering of a fleet's events, read by the report of an EC2 Fleet's history and by
/// the report of a Spot Fleet request's alike, so neither can page over its events in an order
/// the other does not.
pub fn fleet_history_position(at: SystemTime, recorded: usize) -> String {
    let since_epoch = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let separator = CURSOR_SCOPE_SEPARATOR;
    format!(
        "{:020}.{:09}{separator}{recorded:020}",
        since_epoch.as_secs(),
        since_epoch.subsec_nanos()
    )
}

impl ComAmazonawsEc2Amazonec2State {
    /// Record one event in the history of the EC2 Fleet `fleet_id` names, if this store still
    /// holds that fleet.
    ///
    /// How a writer that reaches a fleet BY ID records an event, so every such event is
    /// appended the same way and in the order the events happened. A writer that already holds
    /// the record itself — [`FleetRecord::apply_due_move`], which is handed the record rather
    /// than the store, and [`Self::delete_fleet`], which has just written it — appends to
    /// [`FleetRecord::history`] directly; both append the same [`FleetHistoryEvent`], which is
    /// where the shape of an event is decided.
    pub(super) fn record_fleet_history_event(&mut self, fleet_id: &str, event: FleetHistoryEvent) {
        if let Some(record) = self.fleets.get_mut(fleet_id) {
            record.history.push(event);
        }
    }

    /// Record the launch or termination of one instance in the history of the EC2 Fleet that
    /// PLACED it, if a fleet of this store placed it at all.
    ///
    /// An instance reaches a fleet's history through the placement the fleet launched it with
    /// ([`FleetInstanceRecord`]), which is the one way a fleet's instances are reached anywhere
    /// in this store: an instance a caller launched itself, or one another kind of fleet
    /// placed, belongs to no EC2 Fleet and so records nothing here.
    pub(super) fn record_fleet_instance_event(
        &mut self,
        instance_id: &str,
        event_sub_type: &'static str,
        at: SystemTime,
    ) {
        let placed_by = self
            .fleets
            .iter()
            .find(|(_, record)| {
                record
                    .instances
                    .iter()
                    .any(|placed| placed.instance_id == instance_id)
            })
            .map(|(fleet_id, _)| fleet_id.clone());
        if let Some(fleet_id) = placed_by {
            self.record_fleet_history_event(
                &fleet_id,
                fleet_instance_change_event(event_sub_type, instance_id, at),
            );
        }
    }
}
