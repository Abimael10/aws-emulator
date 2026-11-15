// SPDX-License-Identifier: CC0-1.0

//! The zone another emulated service created and MANAGES, which every hosted-zone read
//! reports as `LinkedService`.
//!
//! `HostedZone$LinkedService` is "if the hosted zone was created by another service, the
//! service that created the hosted zone", and it names that service's own principal
//! (`ServicePrincipal`) and the resource the zone was created for (`Description`). A private
//! DNS namespace of Cloud Map is the standing case: the namespace IS a private hosted zone
//! Cloud Map created and manages for it, so every hosted-zone read of that zone reports
//! Cloud Map as its linked service and the namespace it belongs to.
//!
//! The managing service records that here, on the zone THIS service minted the id of, right
//! after its `CreateHostedZone` answered: the key is a Route 53 zone id, so nothing a caller
//! supplies decides which zone carries the marker and no request can claim one. Reached the
//! way every other fact one emulated service writes and another reads is reached — through
//! the one shared engine, with this service's own store as the single home of it — so the
//! zone and its linked service cannot disagree.
//!
//! Only this store's lock is taken, and only for the one write, so the calling service holds
//! no lock of its own while it runs.

/// Record that `service_principal` created and manages the hosted zone `zone_id` for the
/// resource `description` names (its ARN).
///
/// `false` when no zone is stored under that id — the zone was deleted meanwhile, or this
/// engine carries no Route 53 store at all — in which case nothing is written and nothing is
/// reported: a linked service is a fact of a zone, and a zone that is not there has none.
pub fn link_zone_to_service<Eng: Send + Sync + 'static>(
    engine: &Eng,
    zone_id: &str,
    service_principal: &str,
    description: &str,
) -> bool {
    let Some(state) = crate::state::service_state(engine) else {
        return false;
    };
    let mut store = state.lock();
    let Some(zone) = store.zones.get_mut(crate::state::normalize_zone_id(zone_id)) else {
        return false;
    };
    zone.linked_service = Some(crate::ops::types::LinkedService {
        service_principal: Some(service_principal.to_string()),
        // `LinkedService$Description`, "if the health check or hosted zone was created by
        // another service, an optional description that can be provided by the other
        // service": the resource the zone was created for.
        description: Some(description.to_string()),
    });
    true
}
