// SPDX-License-Identifier: CC0-1.0

//! The rules a state machine alias's ROUTING CONFIGURATION must satisfy, as the service
//! words each of them.
//!
//! One concern, split out of [`super`]: a routing configuration is read by every
//! operation that points an alias ([`super::ComAmazonawsSfnAwsstepfunctionsState::read_routing_configuration`]
//! judges it), and each of its rules is refused in one sentence — the rule the
//! configuration broke, then the references or weights the request submitted, in the
//! order it listed them — so the sentences live together and no two of them can drift
//! apart.

use super::*;

/// What the modelled `ValidationException` says when the weights of a routing
/// configuration do not add up to [`ROUTING_WEIGHT_TOTAL`].
///
/// RECORDED text: the service states the RULE the configuration broke — "the sum of
/// the weights in the routing configuration must be equal to 100" — and then lists,
/// in brackets, the weights the request submitted, in the order it listed them, so a
/// caller sees which numbers it sent. The listed values are the request's own; the
/// total they add up to is not stated, because the weights are what the caller has
/// to change.
pub fn routing_weight_sum_message(weights: &[i32]) -> String {
    format!(
        "Sum of routing configuration weights must equal {ROUTING_WEIGHT_TOTAL}. Received: [{}]",
        weights
            .iter()
            .map(i32::to_string)
            .collect::<Vec<String>>()
            .join(", ")
    )
}

/// What the modelled `ValidationException` says when a routing configuration names
/// versions of more than one state machine.
///
/// RECORDED text, worded as the other rules of the same configuration are: the service
/// states the RULE the configuration broke — "if you specify the ARN of a second
/// version, it must belong to the same state machine as the first version" — and then
/// lists the references the request submitted, in the order it listed them. An alias is
/// a pointer into ONE state machine's versions, so a configuration naming two of them
/// names no state machine the alias could belong to.
pub fn routing_one_state_machine_message(routing: &[RoutingConfigurationListItem]) -> String {
    format!(
        "Routing configuration must contain state machine version ARNs for the same state \
         machine. Received: [{}]",
        routing_version_arns(routing)
    )
}

/// What the modelled `ValidationException` says when an entry of a routing
/// configuration names something that is not a state machine VERSION.
///
/// RECORDED text, worded as the other rules of the same configuration are
/// ([`routing_weight_sum_message`], [`routing_duplicate_version_message`]): the service
/// states the RULE the configuration broke and then lists, in brackets, the references
/// the request submitted, in the order it listed them. An alias routes traffic between
/// VERSIONS, so an unqualified ARN, an alias ARN and a map-run labeled one each name
/// something an alias cannot route to.
pub fn routing_not_a_version_message(routing: &[RoutingConfigurationListItem]) -> String {
    format!(
        "Routing configuration must contain state machine version ARNs. Received: [{}]",
        routing_version_arns(routing)
    )
}

/// What the modelled `ValidationException` says when a routing configuration names one
/// state machine version more than once.
///
/// RECORDED text, worded as [`routing_weight_sum_message`] is for the other rule of the
/// same configuration: the service states the RULE the configuration broke and then
/// lists, in brackets, the version ARNs the request submitted, in the order it listed
/// them, so a caller sees which references it sent. Each entry of the configuration is
/// the SHARE of the traffic one version takes, so two entries naming one version
/// declare two shares of the same thing.
pub fn routing_duplicate_version_message(routing: &[RoutingConfigurationListItem]) -> String {
    format!(
        "Routing configuration must contain distinct state machine version ARNs. Received: [{}]",
        routing_version_arns(routing)
    )
}

/// The version references a routing configuration carries, in the order the request
/// listed them, as the sentences above list them.
fn routing_version_arns(routing: &[RoutingConfigurationListItem]) -> String {
    routing
        .iter()
        .map(|item| item.state_machine_version_arn.clone())
        .collect::<Vec<String>>()
        .join(", ")
}

/// What the modelled `ResourceNotFound` says when a routing configuration names a
/// state machine version that was never published.
///
/// The alias operations do not resolve a routing configuration entry by a generic
/// resource lookup: the rule being enforced is the one the routing configuration
/// itself carries — every entry must reference a version that already exists — so
/// the refusal states THAT rule, in a sentence of its own that no value of the
/// request appears in. RECORDED text.
pub const ROUTING_CONFIGURATION_VERSION_NOT_FOUND: &str =
    "Routing configuration must contain state machine version ARNs that already exist.";
