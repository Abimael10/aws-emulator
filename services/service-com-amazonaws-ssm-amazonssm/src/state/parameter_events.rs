// SPDX-License-Identifier: CC0-1.0

//! The EventBridge event Parameter Store publishes when a parameter is CREATED, UPDATED or
//! DELETED.
//!
//! # The rule this module implements
//!
//! Parameter Store is an event SOURCE, not only a control plane a client reads: every write
//! that changes which parameter versions an account holds — a put that creates a parameter,
//! a put that stores another version of one, a delete that removes one — is published onto
//! the account's default event bus ([`DEFAULT_EVENT_BUS`]) under the `source` this service
//! owns ([`EVENT_SOURCE`]) and the `detail-type` of a parameter change
//! ([`PARAMETER_STORE_CHANGE`]), carrying the changed parameter's ARN in the envelope's
//! `resources` and, in its `detail`, that parameter's name, its type, the operation the
//! write performed ([`OPERATION_CREATE`], [`OPERATION_UPDATE`], [`OPERATION_DELETE`]) and
//! its description. A rule on that bus whose `EventPattern` matches then delivers the
//! envelope to that rule's targets, which is how a configuration change starts a function
//! or a workflow without the client calling anything else.
//!
//! # How a write becomes an event
//!
//! Nothing here decides WHAT changed: the operation that changed the store does, and what it
//! recorded is the fact an event reports — so each event is built from the stored record the
//! operation's own answer is built from, and a write nothing performed publishes nothing. A
//! refused call changes no parameter, and so publishes no event either: only the writes an
//! operation really carried out reach [`publish`]. A read publishes nothing at all.
//!
//! Publication is made with NO store lock held: EventBridge is reached the way every
//! emulated service reaches another, through the framework's in-process callback
//! ([`::service_common::eventbridge_client`]), so this crate names no other service's crate.
//! It is best effort — an emulator with no EventBridge mounted has nowhere to publish — so a
//! failure is logged and the operation's own answer stands.

use ::std::time::SystemTime;

/// The `source` of every event Parameter Store publishes.
///
/// EventBridge names the publishing service in an event's `source` as `aws.<service>`, and
/// this service's namespace is the one its ARNs carry (`ssm`).
pub const EVENT_SOURCE: &str = "aws.ssm";

/// The `detail-type` of the event published when a parameter is created, updated or deleted.
pub const PARAMETER_STORE_CHANGE: &str = "Parameter Store Change";

/// The event bus this service publishes onto.
///
/// The events an AWS service generates go to the account's default event bus, which is the
/// bus a rule carrying no `EventBusName` is created on.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// The `detail.operation` of a put that created a parameter the store did not hold.
pub const OPERATION_CREATE: &str = "Create";

/// The `detail.operation` of a put that stored another version of a parameter the store
/// already held.
pub const OPERATION_UPDATE: &str = "Update";

/// The `detail.operation` of a delete that removed a parameter from the store.
pub const OPERATION_DELETE: &str = "Delete";

/// How many entries one `PutEvents` request carries.
///
/// The bound the service this publishes to declares: its `PutEventsRequestEntryList` is
/// `@length(min: 1, max: 10)`, so a call that changed more parameters than that publishes
/// them in several requests rather than one the service would refuse.
const MAX_PUT_EVENTS_ENTRIES: usize = 10;

/// One change to one parameter that an operation really carried out, waiting to be
/// published.
///
/// Every field is read off the record the operation wrote or removed, so the event reports
/// the parameter that a read of the same instant reports and never a second, invented copy
/// of it.
#[derive(Debug, Clone)]
pub struct ParameterChange {
    /// The parameter's name as the store holds it ([`super::reported_name`]), which is what
    /// every read of it reports and what its ARN is built from.
    pub name: String,
    /// The parameter's `Type` — a `ParameterType` value — as the version the write left
    /// behind carries it, which for a delete is the version it removed.
    pub type_: String,
    /// [`OPERATION_CREATE`], [`OPERATION_UPDATE`] or [`OPERATION_DELETE`].
    pub operation: &'static str,
    /// The parameter's description, when it carries one: `Description` is optional, so a
    /// parameter without one states no `description` rather than an empty one.
    pub description: Option<String>,
    /// WHEN the change was made — the instant the operation recorded, not the instant the
    /// event happened to be published.
    pub at: SystemTime,
}

impl ParameterChange {
    /// The event `detail` this change is reported in.
    ///
    /// Public because it IS the contract: the members a rule's `EventPattern` filters on,
    /// which is what a suite holds the published event to.
    #[must_use]
    pub fn detail(&self) -> ::serde_json::Value {
        let mut detail = ::serde_json::Map::new();
        detail.insert(
            "operation".to_string(),
            ::serde_json::Value::String(self.operation.to_string()),
        );
        detail.insert(
            "name".to_string(),
            ::serde_json::Value::String(self.name.clone()),
        );
        detail.insert(
            "type".to_string(),
            ::serde_json::Value::String(self.type_.clone()),
        );
        if let Some(description) = &self.description {
            detail.insert(
                "description".to_string(),
                ::serde_json::Value::String(description.clone()),
            );
        }
        ::serde_json::Value::Object(detail)
    }
}

/// Publish the parameter changes `changes` reports onto the account's default EventBridge
/// bus, each naming the parameter it changed in `region`.
///
/// Called with NO store lock held, after the operation has recorded what it did: a
/// publication reaches EventBridge's own handler, which matches the event against the
/// account's rules and delivers it to their targets — and a target may call back into this
/// service, so holding this store's lock across it would deadlock the very call the event
/// set off.
pub async fn publish(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    changes: &[ParameterChange],
) {
    if changes.is_empty() {
        return;
    }
    let client = ::service_common::eventbridge_client::eventbridge_client(callback, region);
    for window in changes.chunks(MAX_PUT_EVENTS_ENTRIES) {
        let mut request = client.put_events();
        for change in window {
            let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
                .event_bus_name(DEFAULT_EVENT_BUS)
                .source(EVENT_SOURCE)
                .detail_type(PARAMETER_STORE_CHANGE)
                // The event's `time` is WHEN the change was made:
                // `PutEventsRequestEntry$Time` is "the time stamp of the event, per
                // RFC3339. If no time stamp is provided, the time stamp of the PutEvents
                // call is used."
                .time(::aws_sdk_eventbridge::primitives::DateTime::from(change.at))
                // "The Amazon Resource Names (ARNs) that identify the resources that are
                // involved in the event" — the parameter this change was made to, named by
                // the one ARN every read of it reports ([`super::parameter_arn`]).
                .resources(super::parameter_arn(region, &change.name))
                .detail(change.detail().to_string())
                .build();
            request = request.entries(entry);
        }
        if let Err(error) = request.send().await {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a Parameter Store change could not be published to EventBridge"
            );
        }
    }
}
