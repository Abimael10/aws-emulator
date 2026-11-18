// SPDX-License-Identifier: CC0-1.0

//! The Deployment entity: the record `CreateDeployment` persists and `GetDeployment` and
//! `ListDeployments` read back.
//!
//! Kept beside [`super`]'s policy record rather than inside any one `src/ops/*.rs`, so
//! every deployment operation agrees on one copy of the record, the id minting and the
//! `deploymentIdentifier` resolution — the lookup a read's 200 and its modelled
//! `ResourceNotFoundException` are both decided by.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{
    ComAmazonawsNetworksecuritymanagerPiccolocustomerapiserviceState as State, mint_entity_id,
    to_date_time,
};
use crate::ops::types::{
    AssociatedPolicy, AssociatedScope, DeploymentConfiguration, DeploymentSummary, PolicyReference,
    ScopeReference,
};

/// `ServiceResourceType` naming a deployment, for the `resourceId`/`resourceType` members
/// the modelled `ResourceNotFoundException` / `ServiceQuotaExceededException` shapes
/// carry. One of `ServiceResourceType`'s modelled values.
pub const RESOURCE_TYPE_DEPLOYMENT: &str = "Deployment";

/// The most deployments this emulator stores, after which `CreateDeployment` answers with
/// the modelled `ServiceQuotaExceededException` (HTTP 402) instead of growing
/// [`State::deployments`] for as long as a caller keeps asking. A CHOSEN emulator
/// capacity, not a quota this service documents: what matters is that the store has a
/// ceiling and that reaching it is refused through a modelled error rather than by
/// exhausting the host.
pub const MAX_DEPLOYMENTS: usize = 100;

/// The largest page `ListDeployments` can answer with, and so the page size used when the
/// caller sends no `maxResults`: `DeploymentSummaryList` is modelled `@length(max: 100)`,
/// so a larger page could not be serialized at all.
pub const MAX_DEPLOYMENT_PAGE: usize = 100;

/// One persisted deployment: every member `GetDeployment` answers with, plus the
/// bookkeeping the lifecycle needs (`tags`, `created_at`).
#[derive(Debug, Clone)]
pub struct Deployment {
    pub deployment_id: String,
    pub deployment_arn: String,
    pub deployment_name: String,
    pub deployment_description: Option<String>,
    /// `EntityStatus` — [`super::STATUS_ACTIVE`] or [`super::STATUS_DRAFT`], computed
    /// from `isPublished` by [`super::status_for`].
    pub status: String,
    /// The `DeploymentConfiguration` the caller supplied, stored as given.
    pub deployment_configuration: DeploymentConfiguration,
    /// The policies this deployment is associated with, as the output member
    /// `associatedPolicyList` returns them (`AssociatedPolicy` carries the policy's ARN).
    pub associated_policy_list: Vec<AssociatedPolicy>,
    /// The one scope this deployment is associated with: "A deployment has exactly one
    /// scope" (`AssociatedScopeList` is modelled `@length(1..1)`).
    pub associated_scope_list: Vec<AssociatedScope>,
    /// Rendered as the modelled `EntityVersion` (`^[1-9][0-9]*$`) by
    /// [`Deployment::version_string`]; starts at 1 and is bumped by every update.
    pub version: u32,
    /// `UpdateToken` — a UUID rotated on every mutation, so a caller holding a stale one
    /// is refused with `ConflictException`.
    pub update_token: String,
    pub is_snapshot: bool,
    pub has_published_version: bool,
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl Deployment {
    /// The modelled `EntityVersion` rendering of [`Deployment::version`].
    pub fn version_string(&self) -> String {
        self.version.to_string()
    }

    /// `updatedAt` as the modelled `DateTimestamp`.
    pub fn updated_at_timestamp(&self) -> ::aws_smithy_types::DateTime {
        to_date_time(self.updated_at)
    }

    /// This deployment as the `DeploymentSummary` `ListDeployments` returns.
    pub fn summary(&self) -> DeploymentSummary {
        DeploymentSummary {
            deployment_id: self.deployment_id.clone(),
            deployment_arn: self.deployment_arn.clone(),
            deployment_name: Some(self.deployment_name.clone()),
            status: Some(self.status.clone()),
            version: Some(self.version_string()),
            has_published_version: Some(self.has_published_version),
            updated_at: Some(self.updated_at_timestamp()),
        }
    }
}

impl State {
    /// A fresh `DeploymentId`, minted exactly as a `PolicyId` is (see [`mint_entity_id`]):
    /// it satisfies the modelled `^[a-z0-9]{1,64}$` and cannot collide with an id this
    /// store already handed out.
    pub fn mint_deployment_id(&mut self) -> String {
        mint_entity_id(&mut self.next_deployment_seq)
    }

    /// The `deploymentId` a `deploymentIdentifier` names — the deployment's ARN, as the
    /// model documents it ("This is the deployment's Amazon Resource Name (ARN)"), or the
    /// bare id — or `None` when no stored deployment answers to it.
    pub fn resolve_deployment_id(&self, identifier: &str) -> Option<String> {
        if self.deployments.contains_key(identifier) {
            return Some(identifier.to_string());
        }
        self.deployments
            .values()
            .find(|deployment| deployment.deployment_arn == identifier)
            .map(|deployment| deployment.deployment_id.clone())
    }

    /// The deployment a `deploymentIdentifier` names.
    pub fn deployment_by_identifier(&self, identifier: &str) -> Option<&Deployment> {
        let id = self.resolve_deployment_id(identifier)?;
        self.deployments.get(&id)
    }
}

/// The `associatedPolicyList` entry a request's `PolicyReference` becomes.
///
/// The model: "The identifier of the policy. This is the policy's Amazon Resource Name
/// (ARN), optionally version-qualified" — so the reference carries the ARN and the
/// association echoes it under `policyArn`.
pub fn associated_policy_of(reference: &PolicyReference) -> AssociatedPolicy {
    AssociatedPolicy {
        policy_arn: reference.policy_identifier.clone(),
    }
}

/// The `associatedScopeList` entry a request's `ScopeReference` becomes. See
/// [`associated_policy_of`]: "The identifier of the scope. This is the scope's Amazon
/// Resource Name (ARN)."
pub fn associated_scope_of(reference: &ScopeReference) -> AssociatedScope {
    AssociatedScope {
        scope_arn: reference.scope_identifier.clone(),
    }
}

/// Refuse a deployment's references the service could not answer with, returning the
/// offending member and the message naming what is wrong — `None` when both lists are
/// usable. The check itself is [`super::reference_arn_violation`], shared with every
/// other entity that holds references.
pub fn deployment_reference_violation(
    policies: &[PolicyReference],
    scopes: &[ScopeReference],
) -> Option<(&'static str, String)> {
    super::reference_arn_violation(
        policies
            .iter()
            .map(|reference| {
                (
                    "associatedPolicyList",
                    "policyIdentifier",
                    reference.policy_identifier.as_str(),
                )
            })
            .chain(scopes.iter().map(|reference| {
                (
                    "associatedScopeList",
                    "scopeIdentifier",
                    reference.scope_identifier.as_str(),
                )
            })),
    )
}
