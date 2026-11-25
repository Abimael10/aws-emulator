// SPDX-License-Identifier: CC0-1.0

//! The serving stage's own THROTTLE of the method, metered AROUND one invoke of it.
//!
//! A stage's `methodSettings` state a rate-and-burst pair for a method, and what the pair
//! means is [`crate::state::stage_method_throttle`]'s: the bucket the rate refills is the
//! budget every request the method is served takes from, and the burst is how many requests
//! the stage serves at once and how much of that rate the requests that arrive TOGETHER may
//! draw before it has refilled the bucket. That reading needs the request METERED AROUND the
//! answer rather than at a point in it: a submission is one the next arrival arrives together
//! with from the moment it arrives until the stage has answered it, which for an admitted one
//! is after its integration has been performed.
//!
//! So this module hands the serving path a [`Submission`]: asking for one is the arrival,
//! holding it is being in flight, and dropping it is the answer. A submission the throttle
//! refuses is answered the api's own `THROTTLED` gateway response — rendered by the caller,
//! because it is rendered from the api's records — with the method's integration never
//! performed for it.

use crate::state::{
    ComAmazonawsApigatewayBackplanecontrolserviceState as State, StageMethodThrottleKey,
    ThrottleLimits,
};

/// One submission of a throttled method, IN FLIGHT: taken when the request arrives at the
/// throttle and released when the stage has answered it.
///
/// Whether the throttle admitted it is decided once, when it arrives
/// ([`State::stage_method_throttle_arrived`]), and read off [`Submission::admitted`]. A
/// submission of a method whose settings declare no throttle is not one of these at all:
/// nothing is accounted for it.
pub struct Submission {
    /// The store the accounting lives in — a handle, so the slot is released even though the
    /// answer is produced with no lock held.
    state: ::engine::ServiceState<State>,
    /// The metered method: this API, this stage and the method serving the call.
    key: StageMethodThrottleKey,
    /// Whether the throttle admitted this submission.
    admitted: bool,
}

impl Submission {
    /// Whether the stage's throttle for this method admitted the submission.
    pub fn admitted(&self) -> bool {
        self.admitted
    }
}

impl Drop for Submission {
    fn drop(&mut self) {
        self.state
            .lock()
            .stage_method_throttle_answered(&self.key, self.admitted);
    }
}

/// Meter this invoke against the serving stage's throttle for the method it matched: the
/// submission that is now in flight.
///
/// Taken once everything the method declares about the request has admitted it — the route,
/// the authorization and the per-key metering a usage plan asks for — and before the
/// integration is performed, so a method the stage throttles answers the refusal while its
/// twin on the same stage is served, and the integration observes no invocation for a refused
/// request.
pub fn metered(
    state: &::engine::ServiceState<State>,
    key: StageMethodThrottleKey,
    limits: ThrottleLimits,
) -> Submission {
    let admitted = state
        .lock()
        .stage_method_throttle_arrived(&key, limits, ::std::time::Instant::now());
    Submission {
        state: state.clone(),
        key,
        admitted,
    }
}
