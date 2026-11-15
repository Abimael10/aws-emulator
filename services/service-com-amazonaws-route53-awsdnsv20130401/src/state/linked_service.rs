// SPDX-License-Identifier: CC0-1.0

//! What a hosted-zone read reports for a zone another service created and manages.
//!
//! `HostedZone$LinkedService` is "if the hosted zone was created by another service, the
//! service that created the hosted zone": that other service's own principal
//! (`ServicePrincipal`) and the resource it created the zone for (`Description`). A Cloud Map
//! private DNS namespace is the standing case — the namespace IS a private hosted zone Cloud
//! Map created and manages for it.
//!
//! This is the READ side of that marker, which every hosted-zone projection goes through
//! ([`crate::state::StoredZone::to_hosted_zone`]), so a create, a read and a listing cannot
//! report one zone's linked service differently. The WRITE side — how a zone comes to be
//! managed by another service at all — is [`super::managed_zone`].

use crate::ops::types::LinkedService;
use crate::state::StoredZone;

/// The linked service `zone` reports, or `None` for a zone no other service created.
///
/// Read off the zone itself: the marker is a fact of the zone, recorded once by the service
/// that created it, and nothing about the request doing the reading takes part in it.
#[must_use]
pub fn of_zone(zone: &StoredZone) -> Option<LinkedService> {
    zone.linked_service.clone()
}
