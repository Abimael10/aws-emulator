// SPDX-License-Identifier: CC0-1.0

//! The reference graph between this service's entities, and the refusal a delete owes
//! while a reference into the resource it names is still held.
//!
//! A policy associates templates and rules (`associatedTemplateAndRuleList`), a template
//! associates rules (`associatedRuleList`) and a deployment associates policies and a
//! scope (`associatedPolicyList` / `associatedScopeList`). Every one of those members
//! carries the associated resource's ARN, so one lookup over the stores answers "is this
//! ARN still referenced" for any entity — kept here rather than in a `src/ops/*.rs` so
//! every delete that owes the refusal decides it from one copy of the graph.

use super::ComAmazonawsNetworksecuritymanagerPiccolocustomerapiserviceState as State;
use crate::ops::types::AssociatedTemplateOrRule;

/// The `message` this service states when a mutation is refused because another resource
/// still references the one the request names: fixed text for the error class, naming
/// neither the resource nor the holder of the reference, because the modelled
/// `ConflictException` carries those in its own optional `resourceId` / `resourceType`
/// members.
pub const RESOURCE_IN_USE_MESSAGE: &str = "The resource is currently in use and cannot be modified";

impl State {
    /// Whether any stored entity still holds a reference to the resource whose ARN is
    /// `arn` and whose service-generated id is `resource_id`.
    ///
    /// A create refuses an association that is not an ARN (see
    /// [`super::reference_arn_violation`]), so a stored reference normally IS the ARN; the
    /// bare id every read of this service also resolves is accepted here too, so a
    /// reference written that way still pins the resource it names. Either spelling may
    /// carry the version suffix a reference "optionally version-qualified" is written with.
    pub fn is_referenced(&self, arn: &str, resource_id: &str) -> bool {
        let policy_references = self.policies.values().flat_map(|policy| {
            policy
                .associated_template_and_rule_list
                .iter()
                .map(|association| match association {
                    AssociatedTemplateOrRule::templateArn(value)
                    | AssociatedTemplateOrRule::ruleArn(value) => value.as_str(),
                })
        });
        let template_references = self.templates.values().flat_map(|template| {
            template
                .associated_rule_list
                .iter()
                .map(|association| association.rule_arn.as_str())
        });
        let deployment_references = self.deployments.values().flat_map(|deployment| {
            deployment
                .associated_policy_list
                .iter()
                .map(|association| association.policy_arn.as_str())
                .chain(
                    deployment
                        .associated_scope_list
                        .iter()
                        .map(|association| association.scope_arn.as_str()),
                )
        });

        policy_references
            .chain(template_references)
            .chain(deployment_references)
            .any(|reference| names_resource(reference, arn, resource_id))
    }
}

/// Whether `reference` names the resource `arn` and `resource_id` identify: one of those
/// identifiers, on its own or carrying a version suffix.
///
/// A longer value that merely STARTS with the ARN names a different resource — `:` is the
/// ARN field separator as well as the version separator — so the suffix is accepted only
/// when it is an `EntityVersion` and nothing more.
fn names_resource(reference: &str, arn: &str, resource_id: &str) -> bool {
    [arn, resource_id].into_iter().any(|identifier| {
        !identifier.is_empty()
            && (reference == identifier
                || reference
                    .strip_prefix(identifier)
                    .and_then(|suffix| suffix.strip_prefix(':'))
                    .is_some_and(is_entity_version))
    })
}

/// Whether `value` is an `EntityVersion` as the model declares it: `@length(1..10)` and
/// `@pattern("^[1-9][0-9]*$")`.
///
/// The one reading of a version suffix in this service, so the reference match and the
/// identifier reading ([`super::rule_id_in`]) agree on what part of an identifier is one.
pub(super) fn is_entity_version(value: &str) -> bool {
    (1..=10).contains(&value.len())
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
}
