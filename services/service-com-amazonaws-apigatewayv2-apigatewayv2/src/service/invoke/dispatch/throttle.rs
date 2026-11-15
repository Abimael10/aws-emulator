// SPDX-License-Identifier: CC0-1.0

//! The serving stage's THROTTLE, metered around one invoke of a route.
//!
//! A stage's `RouteSettings` state a rate-and-burst pair for a route, and what the pair means
//! is [`crate::state::route_throttle`]'s: the bucket the rate refills is the budget every
//! request the route is served takes from, and the burst is how much of that rate the requests
//! that arrive TOGETHER may draw before it has refilled the bucket. That reading needs the
//! request METERED AROUND the answer rather than at a point in it: a submission is one the
//! next arrival arrives together with from the moment it arrives until the gateway has
//! answered it, which for an admitted one is after its integration has been performed.
//!
//! So this module hands step 7 a [`Submission`]: asking for one is the arrival, holding it is
//! being in flight, and dropping it is the answer. A submission the throttle refuses carries
//! the refusal it is owed — `429` `{"message":"Too Many Requests"}`, the reason-phrase
//! convention every sentence this endpoint answers follows (RFC 6585 §4) — and the route's
//! integration is never reached for it.

use aws_smithy_runtime_api::http::Response as RtResponse;

use super::super::super::data_plane::json_response;
use super::Dispatch;
use crate::state::route_throttle::Limits;
use crate::state::ComAmazonawsApigatewayv2Apigatewayv2State;

/// The status a request the serving stage's route settings throttle is refused with, and
/// the sentence it carries.
const TOO_MANY_REQUESTS_STATUS: u16 = 429;
const TOO_MANY_REQUESTS_MESSAGE: &str = "Too Many Requests";

/// One submission of a metered route, IN FLIGHT: taken when the request arrives at the
/// throttle and released when the gateway has answered it.
///
/// Whether the throttle admitted it is decided once, when it arrives
/// ([`crate::state::route_throttle::RouteThrottles::arrived`]), and read off
/// [`Self::refusal`]. A submission of a route the stage meters nothing on is not one of
/// these at all: nothing is accounted for it.
pub struct Submission {
    /// The store the accounting lives in — a handle, so the slot is released even though
    /// the answer is produced with no lock held.
    state: ::engine::ServiceState<ComAmazonawsApigatewayv2Apigatewayv2State>,
    /// The metered route: the api, the stage serving it and the route key its settings were
    /// stated for.
    api_id: String,
    stage_name: String,
    route_key: String,
    /// Whether the throttle admitted this submission.
    admitted: bool,
    /// The limits it was metered against, for the refusal's log line.
    limits: Limits,
}

impl Submission {
    /// The refusal this submission is owed, or `None` for one the throttle admitted.
    pub fn refusal(&self) -> Option<RtResponse> {
        if self.admitted {
            return None;
        }
        ::tracing::info!(
            api_id = %self.api_id,
            stage = %self.stage_name,
            route_key = %self.route_key,
            rate = self.limits.rate,
            burst = self.limits.burst,
            "refusing an invoke: the serving stage's route settings throttle this route, so \
             its integration is not reached"
        );
        Some(json_response(
            TOO_MANY_REQUESTS_STATUS,
            TOO_MANY_REQUESTS_MESSAGE,
            None,
        ))
    }
}

impl Drop for Submission {
    fn drop(&mut self) {
        self.state.lock().route_throttles.answered(
            &self.api_id,
            &self.stage_name,
            &self.route_key,
            self.admitted,
        );
    }
}

/// Meter this invoke against the serving stage's throttle for its route: the submission that
/// is now in flight, or `None` for a route the stage meters nothing on.
///
/// It is taken once the route's own authorization has admitted the request — the gate
/// `super::super::invoke` applies and the `REQUEST` authorizer's own decision — and before
/// anything the route fronts is reached, so a route the stage throttles answers the refusal
/// while its twin on the same stage is served, and the integration observes no invocation for
/// a refused request.
pub fn metered<Eng: Send + Sync + 'static>(
    engine: &Eng,
    dispatch: &Dispatch,
) -> Option<Submission> {
    let limits = dispatch.throttle?;
    let state = crate::state::service_state(engine)?;
    let admitted = state.lock().route_throttles.arrived(
        &dispatch.request.api_id,
        &dispatch.request.stage_name,
        &dispatch.route_key,
        limits,
        ::std::time::Instant::now(),
    );
    Some(Submission {
        state,
        api_id: dispatch.request.api_id.clone(),
        stage_name: dispatch.request.stage_name.clone(),
        route_key: dispatch.route_key.clone(),
        admitted,
        limits,
    })
}
