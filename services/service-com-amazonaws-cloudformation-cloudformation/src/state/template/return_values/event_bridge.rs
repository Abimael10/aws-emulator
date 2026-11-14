// SPDX-License-Identifier: CC0-1.0

//! The Amazon EventBridge entries of the return-value catalogue: the resource types this
//! emulator provisions in EventBridge, and the values their `Ref` and `Fn::GetAtt` resolve
//! to. One file of [`super`]'s per-type catalogue, re-exported from it so every caller keeps
//! one path for these names.

use super::*;

/// An Amazon EventBridge rule's resource type (the EventBridge model:
/// `cloudFormationName: Events`). This is a type `super::provisioning` creates in
/// EventBridge itself, and "Ref returns the rule name", so the rule a stack's output names
/// is the one `events DescribeRule` answers with.
pub const EVENTS_RULE: &str = "AWS::Events::Rule";

/// An Amazon EventBridge Pipes pipe's resource type (the Pipes model:
/// `cloudFormationName: Pipes`, `arnNamespace: pipes`). This is a type
/// `super::provisioning` creates in EventBridge Pipes itself, and "Ref returns the pipe
/// name" — which is the physical id the naming rules already compute for the type — so the
/// pipe a stack's output names is the one `pipes DescribePipe` answers with, and the `Arn`
/// and `CurrentState` the type publishes are the ones that service reports for it.
pub const PIPES_PIPE: &str = "AWS::Pipes::Pipe";

/// An Amazon EventBridge EVENT BUS's resource type (the EventBridge model:
/// `cloudFormationName: Events`). This is a type `super::provisioning` creates in
/// EventBridge itself, and "Ref returns the event bus name" — the `Name` property, which is
/// the identifier every EventBridge call on a bus takes and so the physical id the naming
/// rules compute for the type — so the bus another resource of the same stack names is the
/// one `events DescribeEventBus` answers with.
pub const EVENTS_EVENT_BUS: &str = "AWS::Events::EventBus";

/// A statement on an event bus's resource policy, as `AWS::Events::EventBusPolicy` declares
/// one. The resource is the GRANT, not a bus: `PutPermission` installs it on the bus the
/// declaration names and `RemovePermission` revokes it, and the type publishes no
/// `Fn::GetAtt` value of its own.
pub const EVENTS_EVENT_BUS_POLICY: &str = "AWS::Events::EventBusPolicy";

/// What `Fn::GetAtt` reads off an `AWS::Events::EventBus`, in the order
/// [`event_bus_attributes`] inserts them.
///
/// The type's documented return values are `Arn`, `Name` and `Policy`. The first two are
/// decided by the declaration — the bus's name is CloudFormation's to decide and its ARN is
/// built from that name — while `Policy` is the document the bus HOLDS, which no declaration
/// states and a newly created bus has none of: it is read off EventBridge's own answer where
/// something has installed one ([`crate::state::provisioner::event_buses`]) rather than
/// computed from the template.
pub(super) const EVENTS_EVENT_BUS_ATTRIBUTES: &[&str] = &["Arn", "Name"];

/// The attributes an `AWS::Events::EventBus` named `name` publishes
/// ([`EVENTS_EVENT_BUS_ATTRIBUTES`]): its `Name`, which is the name the bus is created
/// under, and the ARN EventBridge mints for a bus of that name in `region` — the same ARN
/// that service's own `event_bus_arn` builds, since both are the one account id this
/// emulator mints plus the bus's name.
pub(super) fn event_bus_attributes(region: &str, name: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    attributes.insert("Arn".to_string(), event_bus_arn(region, name));
    attributes.insert("Name".to_string(), name.to_string());
    attributes
}

/// The ARN of the event bus `name` in `region`:
/// `arn:aws:events:<region>:<account>:event-bus/<name>`.
pub fn event_bus_arn(region: &str, name: &str) -> String {
    ::emulator_server::emulator_arn(EVENTS_ARN_NAMESPACE, region, &format!("event-bus/{name}"))
}

/// The ARN namespace Amazon EventBridge mints its resources under: that model's
/// `awsService.arnNamespace`, which is `events`.
pub(super) const EVENTS_ARN_NAMESPACE: &str = "events";

/// The event bus a rule declaring none is bound to — "if you omit this, the default event
/// bus is used" (`PutRule$EventBusName`, `PutPermission$EventBusName`) — and the one bus
/// whose name a rule ARN does not carry a segment for.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// The bus NAME an `EventBusName` property names.
///
/// EventBridge's `EventBusNameOrArn` admits either a bare name or an event-bus ARN, so an
/// ARN is reduced to everything after its `event-bus/` resource-type segment — the whole
/// remainder, not the text after the last `/`, because a bus name may itself carry `/`.
pub fn event_bus_name(declared: &str) -> String {
    match declared.split_once(":event-bus/") {
        Some((_, name)) => name.to_string(),
        None => declared.to_string(),
    }
}
