// SPDX-License-Identifier: CC0-1.0

//! An api-destination invocation the destination's own invocation RATE has deferred: one
//! whose slot is further off than a publisher would wait for, so it is made on a task of its
//! own and recorded when it is made.
//!
//! "The maximum number of requests per second to send to the HTTP invocation endpoint"
//! (`CreateApiDestination$InvocationRateLimitPerSecond`) throttles the destination, and
//! EventBridge DEFERS what it cannot send yet rather than dropping it: a burst larger than
//! the rate is still delivered, every event exactly once, spread over time. `PutEvents`
//! "returns an `EventId` per accepted entry" and nothing about the deliveries, so a
//! deferred invocation is not something its answer waits for — which is why this carries
//! the handover in OWNED form and performs it after the request that matched the event is
//! long answered.
//!
//! What a deferred invocation does when its slot comes is what an immediate one does: the
//! HTTP request to the destination, the retries the target's `RetryPolicy` allows, the dead
//! letter of an event no attempt delivered, the rule's invocation metrics and the entry in
//! the invocation log. Nothing is invented for it and nothing is skipped.

use super::*;

/// One api-destination invocation waiting for its slot, in the owned form a task can hold.
pub(super) struct Deferred {
    /// The Region the rule that matched is in, which is the Region the delivery is made in.
    pub(super) region: String,
    pub(super) event_id: String,
    pub(super) bus: String,
    pub(super) rule: String,
    pub(super) rule_arn: String,
    pub(super) target: CompiledTarget,
    /// The whole event envelope, or the text the target asked for instead.
    pub(super) body: String,
    pub(super) trace_header: Option<String>,
    /// When the event was PUBLISHED, which is what the target's event-age bound
    /// is read against: a deferred invocation's age includes the span it waited.
    pub(super) published: ::std::time::SystemTime,
    /// The instant the destination's pacing booked this invocation for.
    pub(super) due: ::std::time::SystemTime,
}

/// The stores a deferred invocation reaches, as the owned handles a task can hold: this
/// service's own, where the invocation is recorded and its waiting place given back, and the
/// two another service owns that one delivery reads.
pub(super) struct DeferredStores {
    pub(super) events: ::engine::ServiceState<crate::state::ComAmazonawsEventbridgeAwseventsState>,
    pub(super) cloudwatch: Option<::engine::ServiceState<crate::state::CloudWatchState>>,
    pub(super) kms:
        Option<::engine::ServiceState<crate::state::target_encryption::KmsState>>,
}

/// Start the task that makes `invocation` when its slot comes: `false` when there is no
/// runtime to start it on, in which case the caller makes the invocation itself rather than
/// losing the event.
///
/// The task is HELD by the store it records into
/// ([`crate::state::api_destination_pacing::InvocationPacing::hold_waiting`]), so a waiting
/// invocation is reachable while it waits instead of being a handle nothing can stop.
pub(super) fn defer(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    stores: DeferredStores,
    invocation: Deferred,
) -> bool {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return false;
    };
    let callback = callback.clone();
    let events = ::engine::ServiceState::clone(&stores.events);
    let task = runtime.spawn(async move { perform(&callback, stores, invocation).await });
    events.lock().api_destination_pacing.hold_waiting(task);
    true
}

/// Wait for the booked slot and then make the invocation, recording what came of it exactly
/// as an immediate delivery records it.
async fn perform(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    stores: DeferredStores,
    invocation: Deferred,
) {
    // The wait is what the pacing booked, re-read against the clock so a task that started
    // late waits only what is left of it.
    let waiting = invocation
        .due
        .duration_since(::std::time::SystemTime::now())
        .unwrap_or_default();
    ::tokio::time::sleep(waiting).await;

    let handover = Handover {
        event_id: &invocation.event_id,
        bus: &invocation.bus,
        rule: &invocation.rule,
        rule_arn: &invocation.rule_arn,
        target: &invocation.target,
        rendered: &invocation.body,
        trace_header: invocation.trace_header.as_deref(),
        published: invocation.published,
    };
    let mut clients = Clients::default();
    // A deferred invocation is ONE delivery of its own, so it carries its own budget: the
    // request that matched the event has been answered, and what it had left is spent. What
    // bounds the aggregate is how many deliveries may be waiting at once
    // ([`crate::state::api_destination_pacing::MAX_DEFERRED_INVOCATIONS`]) — each of them one
    // delivery to one target, which is the unit a per-request budget bounds a fan-out in.
    let mut budget = DeliveryBudget::new();
    let mut spent = Spent::default();
    let outcome = invoke_api_destination(
        callback,
        &handover,
        &invocation.body,
        &mut spent,
        &mut budget,
    )
    .await;
    let dead_lettered = dead_letter_refusal(
        callback,
        &mut clients,
        &invocation.region,
        stores.kms.as_ref(),
        &handover,
        &invocation.body,
        &outcome,
        spent.retries.as_ref(),
    )
    .await;
    count_invocation(
        stores.cloudwatch.as_ref(),
        &handover,
        &outcome,
        spent.attempts,
        dead_lettered,
    );
    let mut store = stores.events.lock();
    // The place this invocation held while it waited is given back on every path, so a
    // delivery that failed does not keep another from being deferred.
    store.api_destination_pacing.release_a_waiting_place();
    store.record_invocation(crate::state::TargetInvocation {
        event_id: invocation.event_id.clone(),
        rule: invocation.rule.clone(),
        target_id: invocation.target.id.clone(),
        target_arn: invocation.target.arn.clone(),
        outcome,
    });
}
