// SPDX-License-Identifier: CC0-1.0

//! What an execution still has IN FLIGHT: the work its run has not been reported as
//! finishing, which is the work a stop catches.
//!
//! # The rule this module implements
//!
//! A run that has LEFT the state it ends at has nothing left to do — the execution's own
//! outcome is what happens next, at that same instant — so a service that has not reported
//! an outcome yet has not reported that state as left either: the exit of the state a run
//! ends at is reported WITH the outcome ([`reportable_before_outcome`]), and until then
//! that state is work in flight. Every other event the run wrote is reported from its own
//! instant on ([`super::Execution::advance_history`]), so what a caller reads is still a
//! prefix of the whole history.
//!
//! What STOPS such a run therefore catches the states it was still inside, and a state a
//! stop catches is left unfinished: it records the event its own kind writes for work the
//! stop prevented ([`states_in_flight`]), which `HistoryEventType` models one of per kind
//! of state that can still be running ([`super::asl::aborted_event`]). That is what a
//! reader of the history — and of the trace built from it ([`super::xray_tracing`]) —
//! attributes the stop to, rather than a state that looks as though it completed.

use super::{
    asl, HistoryEvent, EVENT_EXECUTION_REDRIVEN, EVENT_EXECUTION_STARTED, STATE_ABORTED_SUFFIX,
    STATE_ENTERED_SUFFIX, STATE_EXITED_SUFFIX,
};

/// The bytes a stop may add to a history by recording the states it catches in flight.
///
/// One event per state the run ENTERED, which is the most [`states_in_flight`] can report
/// of a history holding those entries: the states a run is inside at one instant do NOT
/// nest — each unit of a concurrent state's work is inside a state of its own, so a
/// `Parallel` of many branches has one in flight per branch — and each event names the
/// state its entry named.
///
/// Reserved while an execution is still running ([`super::Execution::measure`]), so the
/// stop that writes them can only ever shrink what the record retains and admission stays
/// the one place the store grows.
pub(super) fn stop_reserve_bytes(history: &[HistoryEvent]) -> usize {
    history
        .iter()
        .filter(|event| event.event_type.ends_with(STATE_ENTERED_SUFFIX))
        .map(|event| {
            std::mem::size_of::<HistoryEvent>()
                + asl::MAX_ABORTED_EVENT_BYTES
                + event.state_name.as_ref().map_or(0, String::len)
        })
        .sum()
}

/// How many of the events an owed outcome carries are reported BEFORE that outcome is.
///
/// Every one of them but two. The event the attempt ENDS on is the outcome itself — it is
/// stamped at the instant the outcome comes due, so it is never reported sooner. And the
/// EXIT of the state the run ends at is reported with it: a run that has left that state
/// has done all its work, so reporting the exit any sooner would report a run that has
/// finished and reached no outcome, which is not a state an execution is ever in.
///
/// Zero for an attempt that wrote nothing but its own outcome.
pub(super) fn reportable_before_outcome(events: &[HistoryEvent]) -> usize {
    // The last event is the one the attempt ends on: the walk stamps it at `due_at` and
    // records it last ([`super::plan`]).
    let Some((_outcome, walked)) = events.split_last() else {
        return 0;
    };
    let left_the_state_it_ends_at = walked
        .last()
        .is_some_and(|event| event.event_type.ends_with(STATE_EXITED_SUFFIX));
    walked.len() - usize::from(left_the_state_it_ends_at)
}

/// The states `history` reports as ENTERED and not left, innermost first, each with the
/// event type its own kind records when the work around it is STOPPED.
///
/// Only the CURRENT attempt's events are read: "redriven executions append their event
/// history to the existing event history", and a state an earlier attempt was inside when
/// it failed is not work this attempt has in flight — so the walk begins after the event
/// the last attempt opened with.
///
/// A kind of state that records no such event is left out — a `Pass`, a `Choice`, a
/// `Succeed` and a `Fail` leave in the same instant they are entered, so the service
/// models no aborted event for them ([`asl::aborted_event`]).
pub(super) fn states_in_flight(history: &[HistoryEvent]) -> Vec<(&'static str, String)> {
    let attempt_from = history
        .iter()
        .rposition(|event| {
            event.event_type == EVENT_EXECUTION_STARTED
                || event.event_type == EVENT_EXECUTION_REDRIVEN
        })
        .map_or(0, |index| index + 1);
    // The states this attempt is still inside, as `(the kind of state, its name)`, in the
    // order it entered them.
    let mut open: Vec<(&str, &str)> = Vec::new();
    for event in history.get(attempt_from..).unwrap_or_default() {
        let Some(state_name) = event.state_name.as_deref() else {
            continue;
        };
        if let Some(entered) = event.event_type.strip_suffix(STATE_ENTERED_SUFFIX) {
            open.push((entered, state_name));
            continue;
        }
        // A state that was LEFT is finished, and so is one an earlier stop already
        // closed: neither is work anything still has in flight.
        let closed = event
            .event_type
            .strip_suffix(STATE_EXITED_SUFFIX)
            .or_else(|| event.event_type.strip_suffix(STATE_ABORTED_SUFFIX));
        if let Some(closed) = closed {
            if let Some(index) = open
                .iter()
                .rposition(|(kind, name)| *kind == closed && *name == state_name)
            {
                open.remove(index);
            }
        }
    }
    open.iter()
        .rev()
        .filter_map(|(kind, name)| {
            asl::aborted_event(kind).map(|event_type| (event_type, (*name).to_string()))
        })
        .collect()
}
