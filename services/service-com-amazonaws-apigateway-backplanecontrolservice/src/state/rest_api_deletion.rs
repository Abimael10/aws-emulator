// SPDX-License-Identifier: CC0-1.0

//! The REST API deletions this service has ACCEPTED and not yet finished: which APIs are
//! still being removed, what each one carried when its delete was accepted, and how long
//! that removal takes.
//!
//! `DeleteRestApi` answers 202 — "the request has been accepted for processing but the
//! processing has not been completed" — so the call does not report a finished delete: it
//! reports one the control plane has taken on. The API's own records go the moment the
//! delete is accepted, which is what makes every read of them answer their
//! missing-resource refusal straight away, while the removal itself is still travelling
//! through the control plane for the span below.
//!
//! What that span is FOR is the one thing a client can observe about it: a call made
//! inside it that names the API by id — from a record OUTSIDE the API's own tree, which
//! outlives it — is admitted, because the state that call resolved the id against still
//! held the API. The record it writes then outlives the API it names, which is the
//! orphaned reference the delete's own refusal exists to prevent and that a caller racing
//! the two calls gets instead.
//!
//! So what is kept here is what such a call has to be resolved against: the API id, the
//! STAGE NAMES it carried, and the instant the removal is through. A stage the API never
//! had is refused inside the window exactly as it is outside it — the racing call reached
//! state that held the API's stages, not state that had stopped checking them.

use std::collections::{BTreeMap, BTreeSet};

use super::ComAmazonawsApigatewayBackplanecontrolserviceState;

/// How long the removal `DeleteRestApi` accepts takes to complete.
///
/// API Gateway documents no duration for it — the 202 says the delete is accepted, not how
/// long it travels — so the span is the emulator's own: the three seconds this emulator gives
/// a transition the clock owes, which is long enough that a call racing the delete still
/// meets the removal in flight however loaded the host is, and short enough that nothing a
/// caller does waits on it.
pub const REST_API_DELETION_SPAN: ::std::time::Duration = ::std::time::Duration::from_secs(3);

/// One accepted removal still in flight: what the API carried when the delete was accepted,
/// and when that removal is through.
#[derive(Debug, Clone)]
pub struct AcceptedRestApiDeletion {
    /// The instant the removal completes, after which the id resolves to nothing at all.
    pub done_at: ::std::time::Instant,
    /// The names of the stages the API carried when the delete was accepted — what a call
    /// racing the delete resolves a `stage` against, because that call reached state that
    /// still held them.
    pub stages: BTreeSet<String>,
}

/// The removals in flight, as the state field holds them, keyed by the API id.
pub type RestApiDeletionsInFlight = BTreeMap<String, AcceptedRestApiDeletion>;

impl ComAmazonawsApigatewayBackplanecontrolserviceState {
    /// Record that the removal of the API `rest_api_id`, which carried the stages `stages`,
    /// has been ACCEPTED, due to complete [`REST_API_DELETION_SPAN`] from now.
    ///
    /// Called by the delete itself, with the API's records already gone: what is kept is the
    /// id, the stage names and the instant its removal is through, which is what a call
    /// racing the delete is resolved against. Bounded by the removals in flight — every
    /// entry whose span has passed is forgotten here — so it never grows with the deletes an
    /// account makes.
    pub fn accept_rest_api_delete(&mut self, rest_api_id: &str, stages: BTreeSet<String>) {
        let now = ::std::time::Instant::now();
        self.rest_api_deletions_in_flight
            .retain(|_, accepted| now < accepted.done_at);
        self.rest_api_deletions_in_flight.insert(
            rest_api_id.to_string(),
            AcceptedRestApiDeletion {
                done_at: now + REST_API_DELETION_SPAN,
                stages,
            },
        );
    }

    /// The accepted removal of the API `rest_api_id` while it is still in flight — so a call
    /// naming that id has met an accepted delete rather than an id this account never held.
    pub fn rest_api_deletion_in_flight(
        &self,
        rest_api_id: &str,
    ) -> Option<&AcceptedRestApiDeletion> {
        self.rest_api_deletions_in_flight
            .get(rest_api_id)
            .filter(|accepted| ::std::time::Instant::now() < accepted.done_at)
    }

    /// Whether a `BasePathMapping` may be pointed at the API `rest_api_id`: it resolves to a
    /// stored API, or to one whose accepted removal is still in flight
    /// ([`Self::rest_api_deletion_in_flight`]).
    ///
    /// The one reading of "the string identifier of the associated RestApi" the two
    /// operations that create and repoint that mapping share, so a reference written while a
    /// delete travels is admitted the same way whichever of them wrote it.
    pub fn rest_api_referenceable(&self, rest_api_id: &str) -> bool {
        self.rest_apis.contains_key(rest_api_id)
            || self.rest_api_deletion_in_flight(rest_api_id).is_some()
    }

    /// Whether that mapping may name `stage` of the API `rest_api_id`: a stage of the stored
    /// API, or one the API carried when its accepted removal was taken on.
    ///
    /// The stage half of the same reading: a mapping names "the name of the API's stage that
    /// you want to use for this mapping", so a name no stage of the API answers to is refused
    /// whether the removal is in flight or not — what the window changes is which state the
    /// name is resolved against, never whether it is resolved.
    pub fn rest_api_stage_referenceable(&self, rest_api_id: &str, stage: &str) -> bool {
        if let Some(api) = self.rest_apis.get(rest_api_id) {
            return api.stages.contains_key(stage);
        }
        self.rest_api_deletion_in_flight(rest_api_id)
            .is_some_and(|accepted| accepted.stages.contains(stage))
    }
}
