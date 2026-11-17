// SPDX-License-Identifier: CC0-1.0

//! The Amazon EventBridge destination of an event destination: the event bus a
//! configuration set publishes its email events onto, and the event one notification is
//! published as.
//!
//! `EventBridgeDestination$EventBusArn` is "the ARN of the Amazon EventBridge bus to
//! publish email events to", so a destination of this kind receives each matching email
//! event as ONE event put on that bus. Only the account's DEFAULT bus is supported, which
//! is the one rule every operation that establishes such a destination asks here
//! ([`unsupported_bus_refusal`]). The bus is reached the way any emulated service
//! reaches another — the AWS SDK driven over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]) — so nothing here opens a
//! socket or leaves the host, and the publication is the same best-effort hand-over every
//! other destination kind gets ([`super::notification`]): the message was accepted before
//! any event of it existed, so a bus that takes nothing changes nothing about the send.
//!
//! What the event carries is what the SES event-publishing data format states for a bus:
//! the `source` is this service's own event source ([`EVENT_SOURCE`]), the `detail` is the
//! very notification document every other destination kind of the same event destination
//! receives, and the `detail-type` names the email event the detail is of
//! ([`super::notification::Notification::detail_type`]) — so a rule matching this service's
//! source, or one of its event types, sees the whole envelope the published document is
//! wrapped in.

/// The ARN namespace of Amazon EventBridge, which the Region of the bus a destination
/// names is read out of — the Region the in-process publication is addressed at.
const EVENTS_ARN_NAMESPACE: &str = "events";

/// The resource type an event bus is written under inside an EventBridge ARN
/// (`…:event-bus/<name>`), which the bus a destination names is read as.
const EVENT_BUS_RESOURCE_TYPE: &str = "event-bus";

/// The name of the one event bus of an account this service publishes a configuration
/// set's events onto: the account's DEFAULT bus.
const DEFAULT_EVENT_BUS_NAME: &str = "default";

/// How this service words the refusal of an EventBridge destination naming any bus other
/// than the account's default one.
///
/// Fixed text with no variable part: the one bus that is supported is the default one, so
/// the sentence says what the destination has to name rather than echoing what it named.
pub const UNSUPPORTED_EVENT_BUS: &str = "Invalid Event Bridge destination, must be default bus.";

/// The refusal the EventBridge destination `destination` names a bus this service cannot
/// publish to with, or `None` when it names the account's default bus.
///
/// `EventBridgeDestination$EventBusArn` is "the ARN of the Amazon EventBridge bus to
/// publish email events to. Only the default bus is supported", so the DEFAULT bus is the
/// only bus a destination of this kind may name: any other bus is one this service
/// publishes nothing onto, so a destination naming it is refused rather than established.
/// Asked once here, by every operation that takes an event-destination definition
/// ([`crate::state::checked_event_destination_definition`]), so a create of a destination
/// and an update of the same destination accept exactly the same buses.
///
/// The bus is read as the id of an `event-bus` resource of the EventBridge namespace
/// ([`::service_common::arn`]), the one caller-supplied-ARN parse, so a value that is no
/// EventBridge resource ARN, or that names a resource of another type, names no default
/// bus either.
#[must_use]
pub fn unsupported_bus_refusal(
    destination: &crate::ops::types::EventBridgeDestination,
) -> Option<&'static str> {
    let bus_arn = destination.event_bus_arn.as_str();
    let names_the_default_bus =
        ::service_common::arn::is_resource_arn_of(bus_arn, EVENTS_ARN_NAMESPACE)
            && ::service_common::arn::resource_id_of_type(bus_arn, EVENT_BUS_RESOURCE_TYPE)
                == Some(DEFAULT_EVENT_BUS_NAME);
    (!names_the_default_bus).then_some(UNSUPPORTED_EVENT_BUS)
}

/// The `source` of every email event this service publishes onto an event bus: the
/// reserved event source of Amazon SES, which is what an EventBridge rule over this
/// service's events matches on.
pub const EVENT_SOURCE: &str = "aws.ses";

/// Put `document` on the event bus `destination` names, as one event of `detail_type`.
///
/// A bus this call cannot reach — one of another account, one nothing created, or one whose
/// ARN names no Region — takes nothing, which is logged and changes nothing about the send.
pub async fn put_event(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    destination: &crate::ops::types::EventBridgeDestination,
    detail_type: &str,
    document: &::serde_json::Value,
) {
    let bus_arn = destination.event_bus_arn.as_str();
    // The Region out of the bus's own ARN, under the Region grammar every model states for
    // the field: it is the only part of the destination that reaches the endpoint the
    // in-process call is addressed at, so a value that is no Region label names no bus this
    // can reach. Nothing else the caller supplied becomes an address — the endpoint is built
    // from that label alone ([`::service_common::eventbridge_client`]) and the transport is
    // the callback's own, inside this process.
    let Some(region) = ::service_common::arn::region_of(bus_arn, EVENTS_ARN_NAMESPACE) else {
        ::tracing::debug!(
            bus_arn,
            "an event destination's event bus ARN names no Region, so no event is published to it"
        );
        return;
    };
    // The entry addresses the bus by the ARN the destination named: `EventBusNameOrArn`
    // admits either spelling, and the ARN is what this service holds for the destination.
    let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
        .event_bus_name(bus_arn)
        .source(EVENT_SOURCE)
        .detail_type(detail_type)
        .detail(document.to_string())
        .build();
    let answered = ::service_common::eventbridge_client::eventbridge_client(callback, region)
        .put_events()
        .entries(entry)
        .send()
        .await;
    match answered {
        Ok(answer) if answer.failed_entry_count() == 0 => {}
        Ok(answer) => ::tracing::debug!(
            bus_arn,
            detail_type,
            failed_entry_count = answer.failed_entry_count(),
            "an event destination's event bus refused a published email event"
        ),
        Err(error) => ::tracing::debug!(
            bus_arn,
            detail_type,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "an event destination's event bus did not take a published email event"
        ),
    }
}
