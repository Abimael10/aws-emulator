// SPDX-License-Identifier: CC0-1.0

//! The EventBridge events this service publishes as its SERVICE scheduler acts.
//!
//! # The rule this module implements
//!
//! Amazon ECS is an event SOURCE for the work its SERVICE scheduler does, not only for the
//! tasks that work places ([`super::task_events`]): every action the scheduler takes on a
//! service is published onto the account's default event bus ([`DEFAULT_EVENT_BUS`]) under
//! the `source` this service owns ([`EVENT_SOURCE`]) and the `detail-type` of a service
//! action ([`SERVICE_ACTION_DETAIL_TYPE`]), carrying the action's own name in the `detail`'s
//! `eventName`, how serious it is in `eventType`, the cluster the action happened in in
//! `clusterArn` and the instant it happened at in `createdAt`, with the ARN of the resource
//! the action is about in the envelope's `resources`. A rule on that bus whose
//! `EventPattern` matches then delivers it to the rule's targets, so a client that reacts to
//! a service through EventBridge — alerting on a service that cannot keep its tasks up,
//! remediating one automatically — observes the scheduler's actions without calling Amazon
//! ECS again.
//!
//! WHICH actions: the ones the scheduler takes, each under the name Amazon ECS publishes it
//! by ([`SERVICE_ACTION_SEVERITIES`]). The steady state a service's rollout settles into is
//! [`SERVICE_STEADY_STATE`], and the one a task set of a service whose deployments another
//! controller drives settles into is [`TASKSET_STEADY_STATE`]: both are confirmations the
//! CLOCK materialises, so both are published from the pass that materialises them and
//! neither needs a writer to remember to announce it.
//!
//! # How an action becomes an event
//!
//! Nothing here decides WHEN the scheduler acts: the stored due instants do, through
//! [`ComAmazonawsEcsAmazonec2containerservicev20141113State::advance_clock`]. The pass that
//! materialises an action queues it here
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::record_service_action`]) under
//! the same lock, so an action is published exactly once however it was materialised. The
//! queue it feeds is bounded ([`MAX_PENDING_SERVICE_ACTIONS`], oldest dropped first) because
//! every action appends to it.
//!
//! The queue is drained with no lock held, by the same clock pump that publishes a task's
//! moves ([`super::task_events::drive_task_clock`]) and through the same in-process hop, so
//! this crate names no other service's crate and a service action reaches a rule's targets
//! with no further Amazon ECS call. Publication is best effort — an emulator with no
//! EventBridge mounted has nowhere to publish — so a failure is logged and the operation's
//! own answer stands.

use super::*;

/// The `detail-type` of the event published when the SERVICE scheduler takes an action.
pub const SERVICE_ACTION_DETAIL_TYPE: &str = "ECS Service Action";

/// The `eventType` of an action that reports the scheduler making progress.
pub const SERVICE_ACTION_INFO: &str = "INFO";

/// The `eventName` of the action the scheduler takes when a service's rollout reaches the
/// steady state it was rolling out
/// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::settle_services`]).
pub const SERVICE_STEADY_STATE: &str = "SERVICE_STEADY_STATE";

/// The `eventName` of the action the scheduler takes when a TASK SET reaches a steady state
/// — the same confirmation `TaskSet$stabilityStatus` reports to a caller that reads it.
pub const TASKSET_STEADY_STATE: &str = "TASKSET_STEADY_STATE";

/// How serious each action this service publishes is: the `eventType` that goes with the
/// `eventName`, as the Amazon ECS Developer Guide's service action events record the pair.
///
/// The severity belongs to the action, not to the call that materialised it, so it is read
/// off this table rather than passed in by the pass that records the action.
pub const SERVICE_ACTION_SEVERITIES: [(&str, &str); 2] = [
    (SERVICE_STEADY_STATE, SERVICE_ACTION_INFO),
    (TASKSET_STEADY_STATE, SERVICE_ACTION_INFO),
];

/// How serious the action `event_name` is, or `None` for a name this service does not
/// publish.
#[must_use]
pub fn service_action_event_type(event_name: &str) -> Option<&'static str> {
    SERVICE_ACTION_SEVERITIES
        .iter()
        .find(|(name, _)| *name == event_name)
        .map(|(_, event_type)| *event_type)
}

/// How many service actions the queue holds at once.
///
/// This module's OWN ceiling, not an AWS quota, for the reason the task queue has one
/// ([`super::task_events::MAX_PENDING_EVENTS`]): the queue lives in memory and every action
/// appends to it. The oldest action is dropped first, so the actions the scheduler just took
/// are the ones that survive. It is far above what a realistic burst produces — a rollout
/// contributes one steady state.
pub const MAX_PENDING_SERVICE_ACTIONS: usize = 1_000;

/// One service action waiting to be published, rendered against the records as they stood
/// when the scheduler took it.
#[derive(Debug, Clone)]
pub struct ServiceActionEvent {
    /// The resource the action is about, named by the ARN the envelope's `resources`
    /// carries.
    pub resource_arn: String,
    /// WHEN the scheduler acted — the instant the action fell due at, not the instant some
    /// later call materialised or published it.
    pub at: SystemTime,
    /// The rendered `detail`: the action, as Amazon ECS reports it.
    pub detail: ::serde_json::Value,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Queue the service action `event_name`, taken at `at` on the resource `resource_arn`
    /// of the cluster `cluster_arn`.
    ///
    /// Called by the pass that MATERIALISES the action, under the store's own lock, so the
    /// event is queued exactly where the action happened and no second comparison has to
    /// guess at it. An action this service publishes no name for is not queued: the
    /// `eventType` of an event is the action's own ([`SERVICE_ACTION_SEVERITIES`]), so a name
    /// that table does not carry would have to invent one.
    pub(super) fn record_service_action(
        &mut self,
        resource_arn: &str,
        cluster_arn: &str,
        event_name: &str,
        at: SystemTime,
    ) {
        let Some(event_type) = service_action_event_type(event_name) else {
            ::tracing::warn!(
                "no eventType is published for the ECS service action {event_name}, so it was \
                 not published to EventBridge"
            );
            return;
        };
        let mut detail = ::serde_json::Map::new();
        detail.insert(
            "eventType".to_string(),
            ::serde_json::Value::String(event_type.to_string()),
        );
        detail.insert(
            "eventName".to_string(),
            ::serde_json::Value::String(event_name.to_string()),
        );
        detail.insert(
            "clusterArn".to_string(),
            ::serde_json::Value::String(cluster_arn.to_string()),
        );
        // The instant the action happened at, in the date-time text every AWS event body
        // carries an instant in — the same rendering a task's own event uses.
        if let Some(created_at) = super::task_events::date_time_text(reported_instant(at)) {
            detail.insert(
                "createdAt".to_string(),
                ::serde_json::Value::String(created_at),
            );
        }
        while self.pending_service_action_events.len() >= MAX_PENDING_SERVICE_ACTIONS {
            self.pending_service_action_events.pop_front();
            ::tracing::warn!(
                "the queue of ECS service actions waiting to be published to EventBridge is \
                 full at {MAX_PENDING_SERVICE_ACTIONS}; the oldest action was dropped"
            );
        }
        self.pending_service_action_events
            .push_back(ServiceActionEvent {
                resource_arn: resource_arn.to_string(),
                at,
                detail: ::serde_json::Value::Object(detail),
            });
    }

    /// Take at most `limit` of the actions not yet published, oldest first.
    ///
    /// Taking them out under the lock is what stops two concurrent calls from publishing one
    /// action twice.
    pub(super) fn take_service_action_events(&mut self, limit: usize) -> Vec<ServiceActionEvent> {
        let taking = limit.min(self.pending_service_action_events.len());
        self.pending_service_action_events.drain(..taking).collect()
    }

    /// Whether any service action is still waiting to be published — what keeps the clock
    /// pump awake until the bus has them.
    pub(super) fn has_pending_service_actions(&self) -> bool {
        !self.pending_service_action_events.is_empty()
    }

    /// The earliest instant in the FUTURE at which this store owes a SERVICE-level
    /// transition, if any — one of the instants the clock pump waits for.
    ///
    /// The steady state a rollout is owed, the ROLLBACK the deployment circuit breaker owes a
    /// rollout it failed ([`super::circuit_breaker`]), the stability a task set is owed and
    /// the end of the drain a deleted service is in: each is a transition the clock
    /// materialises with no further call, and each is an action a client watching the bus is
    /// waiting on. Only an instant still to come is answered, as the scheduler's own next step
    /// is ([`Self::next_scheduler_step_due_at`]): an instant already past has been offered to
    /// every pass since it fell due, so waking on it again would materialise nothing while
    /// the records it waits on are not there yet.
    pub(super) fn next_service_transition_due_at(&self, now: SystemTime) -> Option<SystemTime> {
        self.services
            .values()
            .flat_map(|service| {
                [
                    service.deployment_steady_due_at,
                    service.deployment_rollback_due_at,
                    service.drain_due_at,
                ]
            })
            .chain(self.task_sets.values().map(|set| set.stable_due_at))
            .flatten()
            .filter(|due| *due > now)
            .min()
    }
}

/// Publish one drain of service actions onto the bus.
///
/// The hop itself is the one a task's own moves travel ([`super::task_events::publish_events`]):
/// one `PutEvents` per window of entries, best effort, with nothing about an operation's own
/// answer depending on it.
pub(super) async fn publish_service_actions(
    client: &::aws_sdk_eventbridge::Client,
    actions: &[ServiceActionEvent],
) {
    let entries: Vec<(&str, SystemTime, &::serde_json::Value)> = actions
        .iter()
        .map(|action| (action.resource_arn.as_str(), action.at, &action.detail))
        .collect();
    super::task_events::publish_events(client, SERVICE_ACTION_DETAIL_TYPE, &entries).await;
}
