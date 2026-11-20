// SPDX-License-Identifier: CC0-1.0

//! The EventSource registry: the slice of this service's state that
//! `CreateEventSource` mints into and `DescribeEventSource` projects.
//!
//! An EventSource forwards events from an origin onto one event bus, so every
//! stored source names the bus it belongs to and lives in that bus's region and
//! account. Nothing a response carries is computed from the request alone —
//! every member is read back out of here.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{base36, spread, ARN_SERVICE, ComAmazonawsEventbridgev2Awseventsv2State};
use crate::ops::types::{
    AwsServiceEventsSourceConfiguration, EventSourceConfiguration,
    PartnerEventsSourceConfiguration, ResourceNotFoundException,
};

/// The resource-type segment of an `EventSourceArn`, from the modelled pattern
/// `^arn:aws…:events:…:…:event-sourcev2\/(aws\.service|aws\.partner)\/<name>\/[a-z0-9]{25}$`.
pub const ARN_RESOURCE_TYPE_EVENT_SOURCE: &str = "event-sourcev2";

/// Width of the id segment of an `EventSourceArn`: the modelled pattern pins it
/// at exactly `[a-z0-9]{25}`.
pub const EVENT_SOURCE_ID_LEN: usize = 25;

/// The ARN type segment of an EventSource that forwards an AWS service's events.
///
/// `EventSourceArn` documents "The type segment is set by the service
/// (aws.service for AWS service events, aws.partner for partner events) and is
/// not part of the resource's name", and its pattern admits exactly these two
/// values.
pub const ORIGIN_TYPE_AWS_SERVICE: &str = "aws.service";

/// The ARN type segment of an EventSource that forwards a partner's events.
/// See [`ORIGIN_TYPE_AWS_SERVICE`].
pub const ORIGIN_TYPE_AWS_PARTNER: &str = "aws.partner";

/// The `EventSourceType` of a source whose origin is an AWS service's events.
///
/// `EventSourceType` documents "Origin type of an EventSource: an AWS service's
/// events (AWS_SERVICE_EVENTS) or a partner event source's events
/// (PARTNER_EVENTS)"; this is an entry of
/// [`crate::ops::types::EVENT_SOURCE_TYPE_VALUES`].
pub const EVENT_SOURCE_TYPE_AWS_SERVICE_EVENTS: &str = "AWS_SERVICE_EVENTS";

/// The `EventSourceType` of a source whose origin is a partner event source.
/// See [`EVENT_SOURCE_TYPE_AWS_SERVICE_EVENTS`].
pub const EVENT_SOURCE_TYPE_PARTNER_EVENTS: &str = "PARTNER_EVENTS";

/// The most EventSources one emulator process holds.
///
/// This is the EMULATOR's own bound rather than a published AWS quota: without
/// it any caller could grow one process's heap without limit. A create past it
/// answers the `LimitExceededException` this operation models — "The request
/// would exceed a service quota for the account".
pub const MAX_EVENT_SOURCES_PER_EMULATOR: usize = 1_000;

/// Lifecycle state of a stored EventSource, as the `EventSourceState` enum words
/// it on the wire.
///
/// Only `ACTIVE` is modelled here, because it is the only state this slice can
/// produce: `StateReason` documents that "EventSources and subscribers are
/// provisioned synchronously and report failures directly on the request", so a
/// created EventSource is operational as soon as the call that minted it
/// returns. The remaining states belong to operations that are not implemented
/// yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSourceLifecycleState {
    /// `ACTIVE` — the EventSource is forwarding events from its origin.
    Active,
}

impl EventSourceLifecycleState {
    /// The `EventSourceState` member value this state serializes as. It is an
    /// entry of [`crate::ops::types::EVENT_SOURCE_STATE_VALUES`].
    pub fn wire_value(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
        }
    }
}

/// A stored EventSource. Every member of `CreateEventSourceResponse` and
/// `DescribeEventSourceResponse` is either a field here or computed from one.
#[derive(Debug, Clone)]
pub struct EventSource {
    pub arn: String,
    pub name: String,
    /// The `EventBusArn` of the bus this source forwards onto.
    pub event_bus_arn: String,
    pub account_id: String,
    /// The region this source lives in: the region of its bus, since a bus
    /// belongs to exactly one region and the source forwards onto that bus.
    pub region: String,
    pub configuration: EventSourceConfiguration,
    pub description: Option<String>,
    pub state: EventSourceLifecycleState,
    /// Whether the bus owner has withdrawn this source, which `Revoked` reports:
    /// `EventSourceState` documents that "Revocation is reported by the Revoked
    /// flag" rather than by a state of its own.
    pub revoked: bool,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub tags: BTreeMap<String, String>,
    /// The `ClientToken` the create that minted this source carried, if any.
    pub client_token: Option<String>,
}

impl EventSource {
    /// The ARN type segment this source carries, which its configuration
    /// variant determines.
    pub fn origin_type(&self) -> &'static str {
        origin_type_of(&self.configuration)
    }

    /// The `EventSourceType` this source reports, which its configuration
    /// variant determines the same way the ARN type segment does.
    pub fn source_type(&self) -> &'static str {
        source_type_of(&self.configuration)
    }
}

/// The `EventSourceType` for a configuration: an AWS service's events or a
/// partner event source's, as that enum's two values name them.
pub fn source_type_of(configuration: &EventSourceConfiguration) -> &'static str {
    match configuration {
        EventSourceConfiguration::AwsServiceEventsConfiguration(_) => {
            EVENT_SOURCE_TYPE_AWS_SERVICE_EVENTS
        }
        EventSourceConfiguration::PartnerEventsConfiguration(_) => EVENT_SOURCE_TYPE_PARTNER_EVENTS,
    }
}

/// The ARN type segment for a configuration: `aws.service` for an AWS service's
/// events, `aws.partner` for a partner's, as `EventSourceArn` documents.
pub fn origin_type_of(configuration: &EventSourceConfiguration) -> &'static str {
    match configuration {
        EventSourceConfiguration::AwsServiceEventsConfiguration(_) => ORIGIN_TYPE_AWS_SERVICE,
        EventSourceConfiguration::PartnerEventsConfiguration(_) => ORIGIN_TYPE_AWS_PARTNER,
    }
}

/// Mint the ARN of an EventSource:
/// `arn:aws:events:<region>:<account>:event-sourcev2/<type>/<name>/<id>`, the
/// form the `EventSourceArn` pattern pins.
pub fn event_source_arn(
    region: &str,
    origin_type: &str,
    name: &str,
    source_id: &str,
) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("{ARN_RESOURCE_TYPE_EVENT_SOURCE}/{origin_type}/{name}/{source_id}"),
    )
}

/// The EventSource id for `seed`: [`EVENT_SOURCE_ID_LEN`] lowercase base-36
/// digits, as the `EventSourceArn` pattern's `[a-z0-9]{25}` segment requires.
///
/// `seed` comes from
/// [`ComAmazonawsEventbridgev2Awseventsv2State::next_id_seed`], which hands out
/// each value once, and the mapping from seed to id is injective, so no two
/// EventSources of one store can carry the same id.
pub fn new_event_source_id(seed: u64, now: SystemTime) -> String {
    base36(spread(seed, now), EVENT_SOURCE_ID_LEN)
}

/// Whether two EventSource configurations describe the same origin, filter and
/// failure handling.
///
/// `EventSourceConfiguration` is a modelled union with no equality of its own,
/// so this is what lets a create decide whether a repeated `ClientToken`
/// carries the same parameters. Presence is compared as well as value, so an
/// omitted member and a present-but-empty one are not the same configuration.
pub fn same_event_source_configuration(
    left: &EventSourceConfiguration,
    right: &EventSourceConfiguration,
) -> bool {
    match (left, right) {
        (
            EventSourceConfiguration::AwsServiceEventsConfiguration(left),
            EventSourceConfiguration::AwsServiceEventsConfiguration(right),
        ) => {
            left.aws_service == right.aws_service
                && left.pattern == right.pattern
                && same_on_failure(left, right)
        }
        (
            EventSourceConfiguration::PartnerEventsConfiguration(left),
            EventSourceConfiguration::PartnerEventsConfiguration(right),
        ) => {
            left.partner_event_source_arn == right.partner_event_source_arn
                && left.pattern == right.pattern
                && left.partner_bus_kms_key_identifier == right.partner_bus_kms_key_identifier
                && same_on_failure(left, right)
        }
        // Two different variants are two different origins.
        _ => false,
    }
}

/// Whether two configurations carry the same `OnFailureConfiguration`,
/// presence included. Generic over the configuration variant, which is what
/// lets both arms of [`same_event_source_configuration`] use it.
fn same_on_failure<C: HasOnFailure>(left: &C, right: &C) -> bool {
    left.dead_letter_queue_arn() == right.dead_letter_queue_arn()
}

/// The `OnFailureConfiguration` an EventSource configuration variant carries.
trait HasOnFailure {
    /// The dead-letter queue ARN configured for failures, and whether an
    /// `OnFailureConfiguration` was given at all.
    fn dead_letter_queue_arn(&self) -> Option<Option<&str>>;
}

impl HasOnFailure for AwsServiceEventsSourceConfiguration {
    fn dead_letter_queue_arn(&self) -> Option<Option<&str>> {
        self.on_failure_configuration
            .as_ref()
            .map(|config| config.arn.as_deref())
    }
}

impl HasOnFailure for PartnerEventsSourceConfiguration {
    fn dead_letter_queue_arn(&self) -> Option<Option<&str>> {
        self.on_failure_configuration
            .as_ref()
            .map(|config| config.arn.as_deref())
    }
}

/// The `ResourceNotFoundException` every EventSource-addressed operation answers
/// with when the store holds no source at the requested ARN.
///
/// ONE construction, so no two operations word the same refusal differently: a
/// fixed sentence plus a single variable part, the ARN the request addressed.
///
/// The sentence names the KIND of resource that was not found followed by
/// `" not found: "` and the identifier the request supplied, which is how every
/// resource of this service words that refusal (a bus by its name, a policy by
/// its name on its bus, a subscriber and an EventSource by their ARN) — the one
/// thing that tells a caller which of several same-shaped 404s it has.
pub fn event_source_not_found(event_source_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("EventSource not found: {event_source_arn}")),
    }
}

/// The top-level fields of an AWS-service EventSource's forwarding pattern that
/// the service writes itself, so a caller's pattern may not also set them.
///
/// `AwsServiceEventsSourceConfiguration$Pattern` states it: "A filter pattern,
/// as a JSON string, that defines which of the service's events are forwarded to
/// the event bus. Do not include source, account, or region as top-level
/// fields."
pub const SERVICE_SET_PATTERN_FIELDS: [&str; 3] = ["source", "account", "region"];

/// The whole `Message` of the refusal a forwarding `Pattern` that is not a JSON
/// document earns.
///
/// `Pattern` documents its own form — "A filter pattern, as a JSON string" — so
/// a text that is not JSON is a request parameter that is not valid. FIXED text:
/// it quotes neither the submitted pattern (which `Pattern` marks `@sensitive`)
/// nor the reader's own parse position.
pub const PATTERN_NOT_VALID_JSON_MESSAGE: &str = "Pattern is not valid JSON";

/// Why `configuration` is not an EventSource configuration this service can
/// forward with, or `None` when it is one.
///
/// The `Some` is the whole `Message` of the refusal, which is why it names the
/// member the request got wrong. ONE reading of the rule, so the create and the
/// update of an EventSource cannot accept different configurations.
pub fn event_source_configuration_fault(
    configuration: &EventSourceConfiguration,
) -> Option<String> {
    match configuration {
        // An AWS-service source's forwarding rule writes the event's origin
        // itself, so its pattern may not set those fields.
        EventSourceConfiguration::AwsServiceEventsConfiguration(configuration) => {
            forwarding_pattern_fault(
                configuration.pattern.as_deref(),
                &SERVICE_SET_PATTERN_FIELDS,
            )
        }
        // A partner source's `Pattern` reserves no field — its doc states only
        // that it is "A filter pattern, as a JSON string" — so the only thing it
        // is held to is being one.
        EventSourceConfiguration::PartnerEventsConfiguration(configuration) => {
            forwarding_pattern_fault(configuration.pattern.as_deref(), &[])
        }
    }
}

/// Why this forwarding pattern is not one a source can carry, or `None` when it
/// is one.
///
/// Two rules, both the member's own: the text is a JSON document, and it sets
/// none of the top-level fields `service_set` names. A pattern that satisfies
/// both is stored as submitted, because what a pattern selects is read by the
/// shared EventBridge pattern reader when the forwarding rule runs rather than
/// settled here.
fn forwarding_pattern_fault(pattern: Option<&str>, service_set: &[&str]) -> Option<String> {
    // "If no pattern is specified, all events from the service are forwarded":
    // a member the request omits names no filter, and so does one carrying no
    // text at all, which the member's modelled `@length` of 0..3753 admits.
    let pattern = pattern.filter(|pattern| !pattern.is_empty())?;
    let Ok(parsed) = ::serde_json::from_str::<::serde_json::Value>(pattern) else {
        return Some(PATTERN_NOT_VALID_JSON_MESSAGE.to_string());
    };
    // A reserved field is a TOP-LEVEL member, so a document that names no
    // members at all sets none of them.
    let fields = parsed.as_object()?;
    service_set
        .iter()
        .find(|field| fields.contains_key(**field))
        .map(|field| {
            format!(
                "Pattern must not set the key '{field}'; the forwarding rule sets it from the \
                 requested AwsService and the source account and region"
            )
        })
}

impl ComAmazonawsEventbridgev2Awseventsv2State {
    /// The EventSource with this ARN, or `None` when no such source exists.
    pub fn event_source(&self, arn: &str) -> Option<&EventSource> {
        self.event_sources.get(arn)
    }

    /// The EventSource carrying this name in this region.
    ///
    /// `EventSourceName` documents that its grammar mirrors `EventBusName`, and
    /// a bus name is unique per account and region, so a source name is scoped
    /// the same way: at most one source matches, which is why the region is
    /// part of the lookup rather than only of the ARN. A scan is enough at an
    /// emulator's scale and cannot drift out of step with [`Self::event_sources`]
    /// the way a second index could.
    pub fn event_source_by_name(&self, region: &str, name: &str) -> Option<&EventSource> {
        self.event_sources
            .values()
            .find(|source| source.region == region && source.name == name)
    }

    /// The EventSource a create carrying this client token in this region
    /// minted, or `None` when no such source is stored.
    ///
    /// The record is the SOURCE: a client token is read off the source that
    /// carried it, so the replay index is the registry itself — nothing beside
    /// it can grow, and no record can outlive the resource it replays.
    pub fn event_source_by_client_token(
        &self,
        region: &str,
        client_token: &str,
    ) -> Option<&EventSource> {
        self.event_sources.values().find(|source| {
            source.region == region && source.client_token.as_deref() == Some(client_token)
        })
    }

    /// How many EventSources forward onto this bus.
    pub fn event_sources_of_bus(&self, event_bus_arn: &str) -> usize {
        self.event_sources
            .values()
            .filter(|source| source.event_bus_arn == event_bus_arn)
            .count()
    }

    /// Remove the EventSource at this ARN, reporting whether one was there.
    ///
    /// The source IS the record of its own client token, so forgetting it frees
    /// both its name and that token: a later create may take the name again, and
    /// a retry carrying the token mints a new source instead of replaying a
    /// deleted one.
    pub fn remove_event_source(&mut self, arn: &str) -> bool {
        self.event_sources.remove(arn).is_some()
    }

    /// Forget the EventSources of a bus that has finished disappearing.
    ///
    /// An EventSource forwards onto one bus, so it cannot outlive that bus: a
    /// source left behind would name a bus no read can resolve. Its client
    /// token goes with it, because the token is held by the source itself.
    pub fn forget_event_sources_of_bus(&mut self, event_bus_arn: &str) {
        let orphaned: Vec<String> = self
            .event_sources
            .values()
            .filter(|source| source.event_bus_arn == event_bus_arn)
            .map(|source| source.arn.clone())
            .collect();
        for arn in orphaned {
            self.event_sources.remove(&arn);
        }
    }
}
