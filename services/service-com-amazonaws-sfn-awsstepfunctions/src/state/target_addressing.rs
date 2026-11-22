// SPDX-License-Identifier: CC0-1.0

//! WHICH Region a `Task` state's integration call is addressed to.
//!
//! A request that addresses its target by a value carrying the invoked service's own
//! ENDPOINT is made against the Region that endpoint names, which is how one `Task` state
//! reaches a resource of another Region: the call goes to that endpoint, is signed for that
//! Region, and the access request it is subject to is over that resource's ARN in it
//! ([`super::integration::addressed_region`], read off the url alone). The one member of
//! the integrations bound here that carries an endpoint is `SendMessage`'s `QueueUrl`.
//!
//! This emulator answers for EVERY Region at one endpoint, so a queue url it minted for a
//! client carries no Region label at all — the authority is where that client reached this
//! process. Read off the url alone such a call is addressed nowhere, and the Region the
//! caller itself runs in stands in; a queue of ANOTHER Region is then unreachable through
//! the only handle its client was given, and the access request is formed over an ARN in
//! the caller's Region, which no grant on the queue's own ARN covers.
//!
//! So where the url names no endpoint Region, WHERE the queue is held answers it instead
//! — read out of the owning service's own store, through that service's own accessor
//! (`service_com_amazonaws_sqs_amazonsqs::state::queue_addressing::holding_region`), so
//! the queue this service addresses is the queue that service holds. The caller's own
//! Region wins wherever it holds a queue of the name: that is the queue its endpoint
//! serves, and two queues of one name are addressed here by one identical string.
//!
//! A pure READ: it takes the owning store's guard, copies a Region label out and releases
//! it, so nothing a later caller sees is changed.
//!
//! The ONE resolution: the gate that decides whether the execution role may make the call
//! ([`super::task_authorization::role_gate`]) and the hand-over that MAKES it
//! ([`super::integration::Addressing`]) both go through it, so the Region the access
//! request is formed in cannot differ from the Region the call is made in.

use std::sync::Arc;

use ::serde_json::Value;

use super::integration::{addressed_queue_url, addressed_region, Addressing, Call};

/// The Region `call` with `request` is ADDRESSED to, or `None` for a call that is
/// addressed nowhere but where its caller already is.
///
/// `caller_region` is the Region the caller runs in — an execution's own — which is both
/// the Region a call whose target names no endpoint is made in and the queue a region-less
/// url addresses wherever that Region holds one (see the module docs).
#[must_use]
pub fn call_region<Eng: Send + Sync + 'static>(
    engine: &Eng,
    call: &Call,
    request: &Value,
    caller_region: &str,
) -> Option<String> {
    // The url's own endpoint, where it names one: a value that carries the service's
    // regional endpoint states the Region itself, and nothing of this emulator's state can
    // overrule what the caller addressed.
    if let Some(named) = addressed_region(call, request) {
        return Some(named);
    }
    let url = addressed_queue_url(call, request)?;
    ::service_com_amazonaws_sqs_amazonsqs::state::queue_addressing::holding_region(
        engine,
        &url,
        caller_region,
    )
}

/// What a walk resolves each of its calls' Regions through ([`Addressing`]): the
/// resolution above, over the shared engine and the Region the execution runs in.
///
/// A closure for the reason [`super::task_authorization::RoleGate`] is one: the records it
/// reads live in another service's store and are reached through the engine the operation
/// holds, and the walk knows nothing of either.
#[must_use]
pub fn addressing<Eng: Send + Sync + 'static>(engine: &Arc<Eng>, region: &str) -> Addressing {
    let engine = Arc::clone(engine);
    let region = region.to_string();
    Arc::new(move |call: &Call, request: &Value| {
        call_region(&*engine, call, request, &region)
    })
}
