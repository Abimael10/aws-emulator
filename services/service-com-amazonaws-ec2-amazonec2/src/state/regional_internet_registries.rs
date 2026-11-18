// SPDX-License-Identifier: CC0-1.0

//! How a Regional Internet Registry is NAMED where this service reports one.
//!
//! The registries are what the modelled `Rir` enum publishes — `ripe`, `apnic`, `arin`,
//! `lacnic`, `nicbr` (`smithy_query(query="shape",shape="Rir")`) — and a request names one by
//! that member. What IPAM reports back is the registry's OWN identifier, which every registry
//! spells in upper case (`RIPE`, `APNIC`, `ARIN`, `LACNIC`, `NICBR`), so the value a reader of
//! an internet registry association sees is that identifier rather than the member's wire
//! spelling. Writing it once here is what keeps the create that stores it and the comparison a
//! replayed `ClientToken` makes from reading one registry two ways.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

/// The registry `requested` names, as this service reports it.
///
/// The identity of a registry is the registry itself, not the case a request spelled it in, so
/// the one canonical spelling — the registry's own, in upper case — is what is stored and
/// reported. The modelled enum has already held `requested` to a member of its catalog before a
/// handler runs, so this only decides that member's REPORTED spelling; surrounding space a
/// request carried is no part of the name either.
pub fn canonical_rir(requested: &str) -> String {
    requested.trim().to_ascii_uppercase()
}
