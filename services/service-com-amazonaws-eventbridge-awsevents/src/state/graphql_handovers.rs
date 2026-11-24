// SPDX-License-Identifier: CC0-1.0

//! The BACKSTOP on the AppSync handovers of one emulator: how many it makes at once, which
//! keeps a cycle of deliveries finite however it is awaited.
//!
//! A delivery to an AppSync GraphQL API target executes an operation on that API
//! (`crate::ops::put_events::app_sync`), and a resolver of that API may itself resolve
//! against an `AMAZON_EVENTBRIDGE` data source, which publishes onto a bus of this same
//! emulator — whose rules may deliver to that API again. The cycle is a client's own
//! topology rather than a fault, and AWS runs it as fast as its own quotas allow; what this
//! emulator cannot do is run it without end, because every hop of it is a frame on one
//! worker.
//!
//! The handover is a direct in-process call rather than a request through the emulator's
//! front door, so the framework's own callback-recursion stamp
//! (`smithy_server_runtime::CallbackRecursionDepth`) does not travel with it and cannot
//! bound it: it is restored to the depth of the call that OPENED the API's data plane each
//! time a resolver makes a call of its own. What bounds a cycle is therefore the CHAIN's own
//! count, carried on the task each handover is awaited in
//! (`crate::ops::put_events::app_sync`'s `MAX_NESTED_HANDOVERS`), so one chain's depth never
//! decides another publisher's delivery.
//!
//! This is the backstop beside it, for a hop that moved a chain onto another task and so
//! restored that count: a nested handover is in flight for as long as the handover that
//! started it — the one awaits the other — so counting the handovers IN FLIGHT bounds how
//! deep any cycle can nest, whatever it is awaited on. Its ceiling is set far above both the
//! chain bound and the deliveries one emulator makes at once
//! ([`MAX_IN_FLIGHT_HANDOVERS`]), because it is there for the case the chain's own count
//! cannot see.
//!
//! What a delivery past either bound REPORTS is a bound of this emulator's own rather than a
//! refusal AWS produces: nothing is handed over, the event is dead-lettered nowhere and it
//! is counted on no metric (`crate::state::InvocationOutcome::NotAttempted`), the way every
//! other handover this emulator does not make is recorded.

/// How many AppSync handovers this emulator makes at once.
///
/// This emulator's OWN bound, not a limit AWS publishes. It is the backstop the module docs
/// state: a ceiling high enough that the deliveries of unrelated publishers — several
/// requests, a replay, a schedule tick, each of which hands over to one target at a time —
/// cannot reach it, and finite so that a cycle the chain's own count cannot see still ends.
pub const MAX_IN_FLIGHT_HANDOVERS: usize = 64;

/// How many AppSync handovers are in flight in this emulator.
#[derive(Debug, Default)]
pub struct GraphqlHandovers {
    in_flight: usize,
}

impl GraphqlHandovers {
    /// Take a place for one handover, or answer `false` once [`MAX_IN_FLIGHT_HANDOVERS`] are
    /// already in flight — which is the delivery the caller then does not make.
    pub fn take_a_place(&mut self) -> bool {
        if self.in_flight >= MAX_IN_FLIGHT_HANDOVERS {
            return false;
        }
        self.in_flight = self.in_flight.saturating_add(1);
        true
    }

    /// Give the place back, which the handover does however it ended.
    pub fn release_a_place(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }
}

/// One handover's place, held for as long as the handover is being made and given back when
/// this value is dropped — on the error paths and on a cancellation as much as on the
/// ordinary one, which is what keeps the count from drifting up until a restart.
pub struct HandoverPlace(::engine::ServiceState<super::ComAmazonawsEventbridgeAwseventsState>);

impl HandoverPlace {
    /// Take a place in `state`, or `None` once this emulator holds as many handovers as it
    /// makes at once.
    pub fn taken(
        state: &::engine::ServiceState<super::ComAmazonawsEventbridgeAwseventsState>,
    ) -> Option<Self> {
        // The guard is held for the counting alone, never across the handover.
        state
            .lock()
            .graphql_handovers
            .take_a_place()
            .then(|| Self(::engine::ServiceState::clone(state)))
    }
}

impl Drop for HandoverPlace {
    fn drop(&mut self) {
        self.0.lock().graphql_handovers.release_a_place();
    }
}

// By hand, because what is diagnosable about a held place is the count it is one of rather
// than the store handle it gives the place back through.
impl ::std::fmt::Debug for HandoverPlace {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("HandoverPlace")
            .field("in_flight", &self.0.lock().graphql_handovers.in_flight)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: this emulator makes [`MAX_IN_FLIGHT_HANDOVERS`] handovers at once and no more,
    /// which is what makes a cycle of deliveries finite — and a place is given back when the
    /// handover that held it ends, so the bound is a ceiling on what is in flight rather than
    /// a quota that runs out.
    #[test]
    fn a_place_is_held_for_one_handover_and_given_back_with_it() {
        let engine = ::engine::InMemoryEngine::new();
        let state = engine.state::<super::super::ComAmazonawsEventbridgeAwseventsState>();
        // Every place the emulator holds, taken.
        let mut held: Vec<HandoverPlace> = (0..MAX_IN_FLIGHT_HANDOVERS)
            .map(|nesting| {
                HandoverPlace::taken(&state).unwrap_or_else(|| {
                    panic!(
                        "case nesting {nesting}: expected a place while fewer than \
                         {MAX_IN_FLIGHT_HANDOVERS} are in flight"
                    )
                })
            })
            .collect();
        assert!(
            HandoverPlace::taken(&state).is_none(),
            "case \"one past the ceiling\": expected no place once \
             {MAX_IN_FLIGHT_HANDOVERS} handovers are in flight"
        );
        // The handover that held one ends, which is what frees the place for the next.
        held.pop();
        let freed = HandoverPlace::taken(&state);
        assert!(
            freed.is_some(),
            "case \"after one handover ended\": expected its place to be free again, got \
             {freed:?}"
        );
        drop(freed);
        drop(held);
        let after = HandoverPlace::taken(&state);
        assert!(
            after.is_some(),
            "case \"after every handover ended\": expected the count to be back to none, \
             got {after:?}"
        );
    }
}
