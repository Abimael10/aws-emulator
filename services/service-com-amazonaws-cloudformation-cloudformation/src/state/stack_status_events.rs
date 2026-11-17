// SPDX-License-Identifier: CC0-1.0

//! The STACK and RESOURCE STATUS-CHANGE EVENTS this service publishes onto the account's
//! default EventBridge bus.
//!
//! ## The rule this module implements
//!
//! CloudFormation is an event SOURCE for the stacks it deploys, not only a control plane a
//! client polls: every status change it records on a stack — the stack's own
//! (`AWS::CloudFormation::Stack`) and each of its resources' — is published onto the
//! account's default event bus ([`super::stack_set_events::DEFAULT_EVENT_BUS`]) under the
//! `source` this service owns ([`super::stack_set_events::EVENT_SOURCE`]) and the
//! `detail-type` of that kind of change ([`STACK_STATUS_CHANGE`],
//! [`RESOURCE_STATUS_CHANGE`]), in the Region the stack belongs to, naming the stack's id
//! both in the event's `resources` and in `detail.stack-id`. A rule on that bus whose
//! `EventPattern` matches then delivers it to that rule's targets, which is how a client
//! gates on a deployment finishing, or on one rolling back, without polling anything.
//!
//! What each `detail` carries is the recorded event it reports and nothing else: the
//! logical and physical resource the transition is about, the type of that resource, the
//! status it reached and the reason recorded with it under `status-details`, and the token
//! the operation was called with. Every variable part is read off the stored event a
//! `DescribeStackEvents` of the same instant answers from, so an event and a read of the
//! same moment cannot disagree — and because the status is stated under
//! `detail.status-details.status` as a whole string, a rule that requires exactly
//! `ROLLBACK_COMPLETE` receives a failed create's conclusion and nothing of a failed
//! update's `UPDATE_ROLLBACK_COMPLETE`.
//!
//! An event that reports no transition owes nothing: a PRE-DEPLOYMENT VALIDATION failure is
//! a statement about a resource NOTHING was done to ([`super::StoredValidationFailure`]),
//! and it carries no resource status to publish.
//!
//! ## When an event is owed, and when it is published
//!
//! Nothing here decides WHAT changed. Recording the event is what owes the publication
//! ([`super::StoredStack::push_event`] marks it, through [`owed_by`]), so a status nothing
//! reached publishes nothing and a refused call publishes nothing either. The mark travels
//! ON the event ([`super::StoredEvent::owed_bus_event`]) rather than in a second queue, so
//! the one bounded event log per stack is also what bounds this.
//!
//! Publishing is a call to ANOTHER service, which cannot be made where the event is
//! recorded — that is under this store's lock, and a stack's events are recorded as the
//! clock reaches them. [`deliver`] is what makes it: it CLAIMS every event the stacks of a
//! Region owe under one lock, releases it, and publishes what it claimed — the same shape
//! [`super::stack_notifications`] uses for the other fan-out a recorded event owes, and it
//! is driven from the same place ([`super::provisioning::advance_stabilisations_in`]).
//! Claiming is TAKING, so an event is published exactly once however many callers drive it.
//!
//! Publication is BEST EFFORT: an emulator with no EventBridge mounted has nowhere to
//! publish, and no caller is failed because the fan-out could not be made.

use ::std::time::{SystemTime, UNIX_EPOCH};

use super::stack_set_events::{DEFAULT_EVENT_BUS, EVENT_SOURCE, MAX_PUT_EVENTS_ENTRIES};

/// The `detail-type` of a change to the STACK itself.
pub const STACK_STATUS_CHANGE: &str = "CloudFormation Stack Status Change";

/// The `detail-type` of a change to ONE RESOURCE of a stack.
pub const RESOURCE_STATUS_CHANGE: &str = "CloudFormation Resource Status Change";

/// What a transition that recorded no success/failure message states where the reason goes:
/// the member is status TEXT, so an event that reports none states it empty rather than
/// leaving a rule with nothing to match on.
const NO_STATUS_REASON: &str = "";

/// Whether a recorded event owes the bus a publication: one that reports a TRANSITION does,
/// and one that reports a pre-deployment validation failure — about a resource nothing was
/// done to, carrying no resource status — does not.
#[must_use]
pub fn owed_by(event: &super::StoredEvent) -> bool {
    event.validation.is_none() && !event.resource_status.is_empty()
}

/// ONE claimed event: the Region it is published in, the stack it is about, WHAT kind of
/// change it is, the `detail` it carries and WHEN the change happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChangeEvent {
    /// The Region the event is published in: the stack's own, which is the Region the call
    /// that changed it was signed for.
    pub region: String,
    /// The stack's id, which the event names in `resources` and in `detail.stack-id`.
    pub stack_id: String,
    /// The `detail-type` this change is published under ([`STACK_STATUS_CHANGE`] for the
    /// stack's own status, [`RESOURCE_STATUS_CHANGE`] for one of its resources').
    pub detail_type: &'static str,
    /// The event's `detail` ([`detail_of`]).
    pub detail: ::serde_json::Value,
    /// WHEN the change happened — the instant the event was recorded, not the instant the
    /// log happened to be drained.
    pub at: SystemTime,
}

/// The `detail-type` a recorded event is published under: the stack's own status change for
/// an event about the stack, and a resource's for every other.
#[must_use]
pub fn detail_type_of(event: &super::StoredEvent) -> &'static str {
    if event.resource_type == super::STACK_RESOURCE_TYPE {
        STACK_STATUS_CHANGE
    } else {
        RESOURCE_STATUS_CHANGE
    }
}

/// The event `detail` one recorded transition is reported in.
///
/// Public because it IS the contract: the members a rule's `EventPattern` filters on, which
/// is what the suites of this crate and of the router crate hold the published event to. The
/// status of whatever changed is stated under `status-details`, which is where the sibling
/// publishers ([`super::stack_set_events`], [`super::drift_events`]) carry it too.
#[must_use]
pub fn detail_of(event: &super::StoredEvent) -> ::serde_json::Value {
    let mut detail = ::serde_json::Map::new();
    detail.insert(
        "stack-id".to_string(),
        ::serde_json::Value::String(event.stack_id.clone()),
    );
    // "LogicalResourceId": the name the template gave the resource — which, for the stack's
    // own event, is the stack's name.
    detail.insert(
        "logical-resource-id".to_string(),
        ::serde_json::Value::String(event.logical_resource_id.clone()),
    );
    // Stated only where the event HAS one: an event about a resource nothing has created yet
    // names no physical resource.
    if let Some(physical) = &event.physical_resource_id {
        detail.insert(
            "physical-resource-id".to_string(),
            ::serde_json::Value::String(physical.clone()),
        );
    }
    let mut status_details = ::serde_json::Map::new();
    status_details.insert(
        "status".to_string(),
        ::serde_json::Value::String(event.resource_status.clone()),
    );
    status_details.insert(
        "status-reason".to_string(),
        ::serde_json::Value::String(
            event
                .resource_status_reason
                .clone()
                .unwrap_or_else(|| NO_STATUS_REASON.to_string()),
        ),
    );
    detail.insert(
        "status-details".to_string(),
        ::serde_json::Value::Object(status_details),
    );
    detail.insert(
        "resource-type".to_string(),
        ::serde_json::Value::String(event.resource_type.clone()),
    );
    // "All events triggered by a given stack operation are assigned the same client request
    // token": stated for an operation that carried one, left out for one that did not.
    if let Some(token) = &event.client_request_token {
        detail.insert(
            "client-request-token".to_string(),
            ::serde_json::Value::String(token.clone()),
        );
    }
    ::serde_json::Value::Object(detail)
}

/// Claim every status-change event the stacks of `region` owe, and publish them onto that
/// Region's default bus.
///
/// Claimed under one lock and published with it released, exactly as the other fan-out a
/// recorded event owes is ([`super::stack_notifications::deliver`], which is driven from the
/// same place). A publication EventBridge refuses is not retried: the event was owed when it
/// was recorded, and a bus that cannot take it is not a reason to keep it — the refusal is
/// logged instead of being dropped silently.
pub async fn deliver(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<super::ComAmazonawsCloudformationCloudformationState>,
    region: &str,
) {
    let claimed: Vec<StatusChangeEvent> = {
        let mut store = state.lock();
        // Every event the clock has made due is recorded first, so this publishes every
        // change owed AS OF NOW rather than as of the last caller.
        store.settle(SystemTime::now());
        store.claim_status_change_events(region)
    };
    publish(ctx.callback(), region, &claimed).await;
}

/// Publish `events` onto the default bus of `region`.
async fn publish(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    events: &[StatusChangeEvent],
) {
    if events.is_empty() {
        return;
    }
    let client = ::service_common::eventbridge_client::eventbridge_client(callback, region);
    for window in events.chunks(MAX_PUT_EVENTS_ENTRIES) {
        let mut request = client.put_events();
        for event in window {
            let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
                .event_bus_name(DEFAULT_EVENT_BUS)
                .source(EVENT_SOURCE)
                .detail_type(event.detail_type)
                // The event's `time` is WHEN the change happened:
                // `PutEventsRequestEntry$Time` is "the time stamp of the event, per RFC3339.
                // If no time stamp is provided, the time stamp of the PutEvents call is
                // used."
                .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                    epoch_millis(event.at),
                ))
                // "The Amazon Web Services resources, identified by Amazon Resource Name
                // (ARN), which the event primarily concerns": the stack the change is about.
                .resources(&event.stack_id)
                .detail(event.detail.to_string())
                .build();
            request = request.entries(entry);
        }
        if let Err(error) = request.send().await {
            ::tracing::warn!(
                region = %region,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a CloudFormation status change could not be published to EventBridge"
            );
        }
    }
}

/// `at` as the epoch milliseconds the event's `time` carries.
fn epoch_millis(at: SystemTime) -> i64 {
    i64::try_from(
        at.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or_default()
}

impl super::ComAmazonawsCloudformationCloudformationState {
    /// Take every status-change event the stacks of `region` owe, in the order the changes
    /// were recorded.
    ///
    /// Claiming is TAKING: the mark comes off each event as it is claimed, so each one is
    /// built once and published once however many callers drive [`deliver`].
    pub fn claim_status_change_events(&mut self, region: &str) -> Vec<StatusChangeEvent> {
        let mut claimed: Vec<StatusChangeEvent> = Vec::new();
        for stack in self.stacks.iter_mut().filter(|stack| stack.region == region) {
            let stack_region = stack.region.clone();
            for event in &mut stack.events {
                if !::std::mem::take(&mut event.owed_bus_event) {
                    continue;
                }
                claimed.push(StatusChangeEvent {
                    region: stack_region.clone(),
                    stack_id: event.stack_id.clone(),
                    detail_type: detail_type_of(event),
                    detail: detail_of(event),
                    at: event.timestamp,
                });
            }
        }
        claimed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One recorded event, with the members these events are built from.
    fn event(
        resource_type: &str,
        logical: &str,
        physical: Option<&str>,
        status: &str,
        reason: Option<&str>,
        token: Option<&str>,
    ) -> super::super::StoredEvent {
        super::super::StoredEvent {
            event_id: "event-1".to_string(),
            stack_id: "arn:aws:cloudformation:us-east-1:123456789012:stack/a/0001".to_string(),
            stack_name: "a".to_string(),
            operation_id: None,
            logical_resource_id: logical.to_string(),
            physical_resource_id: physical.map(ToString::to_string),
            resource_type: resource_type.to_string(),
            timestamp: SystemTime::UNIX_EPOCH,
            resource_status: status.to_string(),
            resource_status_reason: reason.map(ToString::to_string),
            resource_properties: None,
            client_request_token: token.map(ToString::to_string),
            validation: None,
            failed_validation: false,
            owed_notification: None,
            owed_bus_event: false,
        }
    }

    /// The `detail-type` follows the TYPE of the resource the event is about, and the
    /// `detail` states the members a rule filters on.
    #[test]
    fn each_change_states_its_own_members() {
        let stack_id = "arn:aws:cloudformation:us-east-1:123456789012:stack/a/0001";
        for (recorded, expected_type, expected_detail) in [
            (
                event(
                    super::super::STACK_RESOURCE_TYPE,
                    "a",
                    Some(stack_id),
                    "ROLLBACK_COMPLETE",
                    None,
                    Some("token-1"),
                ),
                STACK_STATUS_CHANGE,
                ::serde_json::json!({
                    "stack-id": stack_id,
                    "logical-resource-id": "a",
                    "physical-resource-id": stack_id,
                    "status-details": { "status": "ROLLBACK_COMPLETE", "status-reason": "" },
                    "resource-type": "AWS::CloudFormation::Stack",
                    "client-request-token": "token-1",
                }),
            ),
            (
                event(
                    "AWS::SQS::Queue",
                    "Queue",
                    None,
                    "CREATE_IN_PROGRESS",
                    Some("Resource creation Initiated"),
                    None,
                ),
                RESOURCE_STATUS_CHANGE,
                ::serde_json::json!({
                    "stack-id": stack_id,
                    "logical-resource-id": "Queue",
                    "status-details": {
                        "status": "CREATE_IN_PROGRESS",
                        "status-reason": "Resource creation Initiated",
                    },
                    "resource-type": "AWS::SQS::Queue",
                }),
            ),
        ] {
            let actual_type = detail_type_of(&recorded);
            assert_eq!(
                actual_type, expected_type,
                "case {recorded:?}: expected detail-type {expected_type:?}, got {actual_type:?}"
            );
            let actual = detail_of(&recorded);
            assert_eq!(
                actual, expected_detail,
                "case {recorded:?}: expected {expected_detail}, got {actual}"
            );
        }
    }

    /// An event that reports a transition owes the bus a publication; one that reports a
    /// pre-deployment validation failure owes nothing.
    #[test]
    fn only_a_transition_owes_the_bus_an_event() {
        let transition = event(
            super::super::STACK_RESOURCE_TYPE,
            "a",
            None,
            "CREATE_IN_PROGRESS",
            None,
            None,
        );
        assert!(
            owed_by(&transition),
            "case {transition:?}: a recorded transition owes the bus one event"
        );

        let mut validation = event(super::super::STACK_RESOURCE_TYPE, "a", None, "", None, None);
        validation.validation = Some(super::super::StoredValidationFailure {
            name: "name".to_string(),
            status: "FAILED".to_string(),
            reason: "why".to_string(),
            path: "/Resources".to_string(),
        });
        assert!(
            !owed_by(&validation),
            "case {validation:?}: a validation failure is no status change, so it owes nothing"
        );
    }
}
