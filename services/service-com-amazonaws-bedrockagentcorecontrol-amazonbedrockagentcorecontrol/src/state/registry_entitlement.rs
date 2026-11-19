// SPDX-License-Identifier: CC0-1.0

//! Whether this account may use the AgentCore Registry at all.
//!
//! The registry plane is an ENTITLED feature: an account holds the AgentCore
//! Registry entitlement or it does not, and one that does not is refused every
//! registry call — deterministically, on every attempt, before anything is
//! recorded. Nothing a client can call grants it: AWS grants it to an account out
//! of band, which is why there is no modelled operation for it and why an
//! account's state starts WITHOUT it ([`Default`]), the position every recorded
//! registry call of this endpoint was made from.
//!
//! [`grant_registry_entitlement`] is the out-of-band seam that stands in for that
//! grant — the same shape of seam as the one another service's store takes for an
//! entitlement a subscription grants outside its own API — so an entitled
//! account's registry plane is reachable and exercisable without the refusal
//! being anything other than this account's real state.
//!
//! WHERE THE REFUSAL IS WORDED: `crate::service::entitlement`, beside the other
//! decision this endpoint takes ahead of a handler, because a refused call must
//! not reach one.

use super::*;

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// Whether this account holds the AgentCore Registry entitlement.
    pub fn holds_registry_entitlement(&self) -> bool {
        self.registry_entitlement
    }

    /// Record that this account holds the AgentCore Registry entitlement,
    /// answering whether it did not hold one before.
    ///
    /// Idempotent: an account holds the entitlement or it does not, so granting
    /// it twice is the same state as granting it once.
    pub fn grant_registry_entitlement(&mut self) -> bool {
        !::std::mem::replace(&mut self.registry_entitlement, true)
    }
}

/// Grant the account this engine holds the state of the AgentCore Registry
/// entitlement, answering whether the engine carried this service's store.
///
/// THE OUT-OF-BAND SEAM. AWS grants this entitlement to an account outside the
/// API — there is no operation for it in this service's model — so this is the
/// only way an account comes to hold one, exactly as the entitlements another
/// service reads are written into its store from outside it. Everything else
/// about the registry plane is driven through the modelled operations.
pub fn grant_registry_entitlement<Eng: Send + Sync + 'static>(engine: &Eng) -> bool {
    match service_state(engine) {
        Some(state) => {
            state.lock().grant_registry_entitlement();
            true
        }
        None => false,
    }
}
