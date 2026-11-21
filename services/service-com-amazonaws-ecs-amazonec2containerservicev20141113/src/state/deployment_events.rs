// SPDX-License-Identifier: CC0-1.0

//! The EventBridge events this service publishes as a service DEPLOYMENT moves through its
//! rollout states.
//!
//! # The rule this module implements
//!
//! A service deployment is a state machine with three states
//! (`Deployment$rolloutState`: "When a service deployment is started, it begins in an
//! `IN_PROGRESS` state. When the service reaches a steady state, the deployment transitions
//! to a `COMPLETED` state. If the service fails to reach a steady state and circuit breaker
//! is turned on, the deployment transitions to a `FAILED` state"), and EVERY state it moves
//! into is published onto the account's default event bus ([`DEFAULT_EVENT_BUS`]) under the
//! `source` this service owns ([`EVENT_SOURCE`]) and the `detail-type` of a deployment's
//! state change ([`DEPLOYMENT_STATE_CHANGE_DETAIL_TYPE`]). The `detail` names the state the
//! rollout moved into in `eventName`, how serious that state is in `eventType`, the rollout
//! that moved in `deploymentId`, the instant it moved at in `updatedAt` and why it is in
//! that state in `reason`; the envelope's `resources` names the SERVICE the rollout belongs
//! to. A rule on that bus whose `EventPattern` matches then delivers it to the rule's
//! targets, so a client that reacts to a deployment through EventBridge — alerting on a
//! rollout that failed, gating a pipeline on one that completed — observes the whole rollout
//! without calling Amazon ECS again.
//!
//! WHICH state publishes WHICH event is [`DEPLOYMENT_STATE_EVENTS`], keyed by the rollout
//! state the API itself reports for the deployment, so the event and `DescribeServices`
//! cannot disagree about where a rollout stands.
//!
//! # How a transition becomes an event
//!
//! Nothing here decides WHEN a rollout moves: the pass that materialises the move queues the
//! event under the store's own lock
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::record_deployment_state_change`]),
//! exactly as a service action is queued ([`super::service_action_events`]) — the call that
//! starts a rollout
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::start_service_deployment`], the
//! one door every rollout of every service is recorded through), the clock pass that settles
//! one into its steady state and the deployment circuit breaker that fails one
//! ([`super::circuit_breaker`]). So a transition is published exactly once however it was
//! materialised, dated by the instant it fell due rather than by whoever happened to call.
//! The queue it feeds is bounded ([`MAX_PENDING_DEPLOYMENT_EVENTS`], oldest dropped first)
//! because every transition appends to it.
//!
//! The queue is drained with no lock held, by the same clock pump that publishes a task's
//! moves ([`super::task_events::drive_task_clock`]) and through the same in-process hop, so
//! this crate names no other service's crate and a deployment transition reaches a rule's
//! targets with no further Amazon ECS call. Publication is best effort — an emulator with no
//! EventBridge mounted has nowhere to publish — so a failure is logged and the operation's
//! own answer stands.

use super::*;

/// The `detail-type` of the event published when a service DEPLOYMENT changes state.
pub const DEPLOYMENT_STATE_CHANGE_DETAIL_TYPE: &str = "ECS Deployment State Change";

/// The `eventType` of a deployment transition that reports progress — the same severity the
/// scheduler's own actions are reported under ([`SERVICE_ACTION_INFO`]).
pub const DEPLOYMENT_EVENT_INFO: &str = SERVICE_ACTION_INFO;

/// The `eventType` of a deployment transition that reports a FAILURE: the state a rollout
/// reaches instead of the steady state it was rolling out is an error for the client
/// watching it, not progress.
pub const DEPLOYMENT_EVENT_ERROR: &str = "ERROR";

/// The `eventName` of the transition a rollout makes when it STARTS, which is the state
/// `Deployment$rolloutState` reports as [`ROLLOUT_IN_PROGRESS`].
pub const SERVICE_DEPLOYMENT_IN_PROGRESS_EVENT: &str = "SERVICE_DEPLOYMENT_IN_PROGRESS";

/// The `eventName` of the transition a rollout makes when the service reaches the steady
/// state it was rolling out ([`ROLLOUT_COMPLETED`]).
pub const SERVICE_DEPLOYMENT_COMPLETED_EVENT: &str = "SERVICE_DEPLOYMENT_COMPLETED";

/// The `eventName` of the transition a rollout makes when it will never reach that steady
/// state ([`ROLLOUT_FAILED`]).
pub const SERVICE_DEPLOYMENT_FAILED_EVENT: &str = "SERVICE_DEPLOYMENT_FAILED";

/// Which event each rollout state is published as: the `eventName` of the transition INTO
/// that state and the `eventType` that goes with it, as the Amazon ECS Developer Guide's
/// service deployment state change events record the pair.
///
/// Keyed by the rollout state the API reports ([`ROLLOUT_IN_PROGRESS`],
/// [`ROLLOUT_COMPLETED`], [`ROLLOUT_FAILED`] — every member of `DeploymentRolloutState`), so
/// a pass that materialises a transition names the state it moved the rollout to and the
/// event it publishes follows from that, rather than each pass choosing a name of its own.
pub const DEPLOYMENT_STATE_EVENTS: [(&str, &str, &str); 3] = [
    (
        ROLLOUT_IN_PROGRESS,
        SERVICE_DEPLOYMENT_IN_PROGRESS_EVENT,
        DEPLOYMENT_EVENT_INFO,
    ),
    (
        ROLLOUT_COMPLETED,
        SERVICE_DEPLOYMENT_COMPLETED_EVENT,
        DEPLOYMENT_EVENT_INFO,
    ),
    (
        ROLLOUT_FAILED,
        SERVICE_DEPLOYMENT_FAILED_EVENT,
        DEPLOYMENT_EVENT_ERROR,
    ),
];

/// The `(eventName, eventType)` the rollout state `rollout_state` is published as, or `None`
/// for a state this service publishes no deployment event for.
#[must_use]
pub fn deployment_state_event(rollout_state: &str) -> Option<(&'static str, &'static str)> {
    DEPLOYMENT_STATE_EVENTS
        .iter()
        .find(|(state, _, _)| *state == rollout_state)
        .map(|(_, event_name, event_type)| (*event_name, *event_type))
}

/// How many deployment transitions the queue holds at once.
///
/// This module's OWN ceiling, not an AWS quota, for the reason the task queue has one
/// ([`super::task_events::MAX_PENDING_EVENTS`]): the queue lives in memory and every
/// transition appends to it. The oldest transition is dropped first, so the moves the live
/// rollouts just made are the ones that survive. It is far above what a realistic burst
/// produces — a rollout contributes two transitions, its start and its end.
pub const MAX_PENDING_DEPLOYMENT_EVENTS: usize = 1_000;

/// One deployment state change waiting to be published, rendered against the records as they
/// stood when the rollout moved.
#[derive(Debug, Clone)]
pub struct DeploymentStateChange {
    /// The SERVICE the rollout belongs to, named by the ARN the envelope's `resources`
    /// carries.
    pub service_arn: String,
    /// WHEN the rollout moved — the instant the transition fell due at, not the instant some
    /// later call materialised or published it.
    pub at: SystemTime,
    /// The rendered `detail`: the transition, as Amazon ECS reports it.
    pub detail: ::serde_json::Value,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Queue the transition of the rollout `deployment_id` of the service `service_arn` INTO
    /// `rollout_state`, which fell due at `at` for the reason `reason`.
    ///
    /// Called by the pass that MATERIALISES the transition, under the store's own lock, so
    /// the event is queued exactly where the rollout moved and no second comparison has to
    /// guess at it. A state this service publishes no event for is not queued: the
    /// `eventName` and `eventType` of an event are the state's own
    /// ([`DEPLOYMENT_STATE_EVENTS`]), so a state that table does not carry would have to
    /// invent them.
    pub(super) fn record_deployment_state_change(
        &mut self,
        service_arn: &str,
        deployment_id: &str,
        rollout_state: &str,
        reason: &str,
        at: SystemTime,
    ) {
        let Some((event_name, event_type)) = deployment_state_event(rollout_state) else {
            ::tracing::warn!(
                "no event is published for the ECS deployment rollout state {rollout_state}, \
                 so the transition of {deployment_id} was not published to EventBridge"
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
            "deploymentId".to_string(),
            ::serde_json::Value::String(deployment_id.to_string()),
        );
        // The instant the rollout moved at, in the date-time text every AWS event body
        // carries an instant in — the same rendering a task's own event uses.
        if let Some(updated_at) = super::task_events::date_time_text(reported_instant(at)) {
            detail.insert(
                "updatedAt".to_string(),
                ::serde_json::Value::String(updated_at),
            );
        }
        // Why the rollout is in that state: the same sentence the API reports as the
        // deployment's `rolloutStateReason`, so the event and a read of the deployment
        // cannot disagree about it.
        detail.insert(
            "reason".to_string(),
            ::serde_json::Value::String(reason.to_string()),
        );
        while self.pending_deployment_state_changes.len() >= MAX_PENDING_DEPLOYMENT_EVENTS {
            self.pending_deployment_state_changes.pop_front();
            ::tracing::warn!(
                "the queue of ECS deployment state changes waiting to be published to \
                 EventBridge is full at {MAX_PENDING_DEPLOYMENT_EVENTS}; the oldest change \
                 was dropped"
            );
        }
        self.pending_deployment_state_changes
            .push_back(DeploymentStateChange {
                service_arn: service_arn.to_string(),
                at,
                detail: ::serde_json::Value::Object(detail),
            });
    }

    /// Take at most `limit` of the transitions not yet published, oldest first.
    ///
    /// Taking them out under the lock is what stops two concurrent calls from publishing one
    /// transition twice.
    pub(super) fn take_deployment_state_changes(
        &mut self,
        limit: usize,
    ) -> Vec<DeploymentStateChange> {
        let taking = limit.min(self.pending_deployment_state_changes.len());
        self.pending_deployment_state_changes
            .drain(..taking)
            .collect()
    }

    /// Whether any deployment transition is still waiting to be published — what keeps the
    /// clock pump awake until the bus has them.
    pub(super) fn has_pending_deployment_state_changes(&self) -> bool {
        !self.pending_deployment_state_changes.is_empty()
    }
}

/// Publish one drain of deployment state changes onto the bus.
///
/// The hop itself is the one a task's own moves travel
/// ([`super::task_events::publish_events`]): one `PutEvents` per window of entries, best
/// effort, with nothing about an operation's own answer depending on it.
pub(super) async fn publish_deployment_state_changes(
    client: &::aws_sdk_eventbridge::Client,
    changes: &[DeploymentStateChange],
) {
    let entries: Vec<(&str, SystemTime, &::serde_json::Value)> = changes
        .iter()
        .map(|change| (change.service_arn.as_str(), change.at, &change.detail))
        .collect();
    super::task_events::publish_events(client, DEPLOYMENT_STATE_CHANGE_DETAIL_TYPE, &entries).await;
}
