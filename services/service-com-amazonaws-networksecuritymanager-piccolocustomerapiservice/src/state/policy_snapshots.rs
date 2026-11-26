// SPDX-License-Identifier: CC0-1.0

//! A Policy's published version and the snapshots taken of it: the records
//! `CreatePolicySnapshot` persists and every policy-shaped response renders.
//!
//! Kept beside [`super`]'s live policy record rather than inside any one `src/ops/*.rs`,
//! so the live/frozen split a snapshot answers with ([`PolicyFacts`]) is decided in one
//! place and every operation that can answer with a snapshot answers alike.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{
    ComAmazonawsNetworksecuritymanagerPiccolocustomerapiserviceState as State, Policy,
    STATUS_ACTIVE, to_date_time,
};
use crate::ops::types::{AssociatedTemplateOrRule, PolicyConfiguration};

/// The most snapshots this emulator keeps of ONE policy, after which
/// `CreatePolicySnapshot` answers with the modelled `ServiceQuotaExceededException`
/// (HTTP 402) instead of growing [`State::policy_snapshots`] for as long as a caller
/// keeps asking. A CHOSEN emulator capacity, not a quota this service documents: what
/// matters is that the store has a ceiling and that reaching it is refused through a
/// modelled error rather than by exhausting the host.
pub const MAX_POLICY_SNAPSHOTS: usize = 100;

/// The `message` this service states when a snapshot is asked of a policy that has never
/// published a version, so there is no published version to copy: fixed text for the
/// error class, naming no identifier, because the modelled `ConflictException` carries
/// the policy in its own `resourceId` / `resourceType` members.
pub const NO_PUBLISHED_VERSION_MESSAGE: &str = "The resource has no published version";

/// One version of a policy: the members an update writes as a unit, frozen as they stood
/// when that version was published.
///
/// A snapshot is "an immutable, versioned copy" of the policy's published version, so the
/// published body has to be kept apart from the live record: a draft update moves the live
/// record on without touching what is published, and a snapshot taken afterwards still
/// copies the published body.
#[derive(Debug, Clone)]
pub struct PolicyVersion {
    /// The `EntityVersion` the live record carried when this version was published.
    pub version: u32,
    pub policy_description: Option<String>,
    pub policy_configuration: Option<PolicyConfiguration>,
    pub associated_template_and_rule_list: Vec<AssociatedTemplateOrRule>,
}

/// One snapshot of a policy: an immutable copy of the policy's published version at the
/// moment the snapshot was taken, plus the bookkeeping a snapshot of its own needs
/// (`tags`, the time it was created, its own `updateToken`).
#[derive(Debug, Clone)]
pub struct PolicySnapshot {
    /// The `policyId` of the policy this is a snapshot of. A snapshot is a version of
    /// that policy, not a policy of its own, so it answers with the same id.
    pub policy_id: String,
    /// The snapshot's own `policyArn`: the policy's ARN carrying the version suffix a
    /// version-qualified identifier of this service is written with (see
    /// [`super::is_entity_version`]), which is what lets a caller read this one version
    /// back while the live policy moves on.
    pub policy_arn: String,
    /// The published body this snapshot froze.
    pub published: PolicyVersion,
    /// `UpdateToken` — minted once, with the snapshot, and never rotated: every read of
    /// an immutable copy returns the same token.
    pub update_token: String,
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
}

/// Every member a policy-shaped response carries, resolved for one policy or for one of
/// its snapshots.
///
/// The one home for the live/frozen split, so `CreatePolicySnapshot` and a read that
/// resolves a version-qualified identifier cannot disagree about what a snapshot holds.
#[derive(Debug, Clone)]
pub struct PolicyFacts {
    pub policy_id: String,
    pub policy_arn: String,
    pub policy_name: String,
    pub policy_description: Option<String>,
    pub status: String,
    pub priority: i32,
    pub associated_template_and_rule_list: Vec<AssociatedTemplateOrRule>,
    pub version: String,
    pub update_token: String,
    pub is_snapshot: bool,
    pub has_published_version: bool,
    pub firewall_type: String,
    pub policy_configuration: Option<PolicyConfiguration>,
    pub updated_at: ::aws_smithy_types::DateTime,
}

impl Policy {
    /// This policy's current body as one version: what publishing it records, and what a
    /// snapshot of the published version copies.
    pub fn version_body(&self) -> PolicyVersion {
        PolicyVersion {
            version: self.version,
            policy_description: self.policy_description.clone(),
            policy_configuration: self.policy_configuration.clone(),
            associated_template_and_rule_list: self.associated_template_and_rule_list.clone(),
        }
    }

    /// Every member a policy-shaped response carries, for the LIVE policy.
    pub fn facts(&self) -> PolicyFacts {
        PolicyFacts {
            policy_id: self.policy_id.clone(),
            policy_arn: self.policy_arn.clone(),
            policy_name: self.policy_name.clone(),
            policy_description: self.policy_description.clone(),
            status: self.status.clone(),
            priority: self.priority,
            associated_template_and_rule_list: self.associated_template_and_rule_list.clone(),
            version: self.version_string(),
            update_token: self.update_token.clone(),
            is_snapshot: self.is_snapshot,
            has_published_version: self.has_published_version,
            firewall_type: self.firewall_type.clone(),
            policy_configuration: self.policy_configuration.clone(),
            updated_at: self.updated_at_timestamp(),
        }
    }
}

impl PolicySnapshot {
    /// Every member a policy-shaped response carries, for this snapshot of `live`.
    ///
    /// What the published version holds comes from the frozen copy — the
    /// `policyConfiguration`, the `associatedTemplateAndRuleList`, the
    /// `policyDescription` and the `version` that was copied — so it stays as it stood
    /// however far the live policy moves on. What belongs to the policy as a whole rather
    /// than to one of its versions is read from the live record, so a snapshot reports the
    /// policy's current `policyName` and `priority` (the number that orders the policy
    /// against its peers). `updatedAt` is "The time when the snapshot was created" (the
    /// model, on this very member), and the copy is of a PUBLISHED version, so its status
    /// is [`STATUS_ACTIVE`].
    pub fn facts(&self, live: &Policy) -> PolicyFacts {
        PolicyFacts {
            policy_id: self.policy_id.clone(),
            policy_arn: self.policy_arn.clone(),
            policy_name: live.policy_name.clone(),
            policy_description: self.published.policy_description.clone(),
            status: STATUS_ACTIVE.to_string(),
            priority: live.priority,
            associated_template_and_rule_list: self
                .published
                .associated_template_and_rule_list
                .clone(),
            version: self.published.version.to_string(),
            update_token: self.update_token.clone(),
            is_snapshot: true,
            has_published_version: true,
            firewall_type: live.firewall_type.clone(),
            policy_configuration: self.published.policy_configuration.clone(),
            updated_at: to_date_time(self.created_at),
        }
    }
}

impl State {
    /// How many snapshots this store holds of `policy_id` — the count [`MAX_POLICY_SNAPSHOTS`]
    /// bounds.
    pub fn policy_snapshot_count(&self, policy_id: &str) -> usize {
        self.policy_snapshots
            .keys()
            .filter(|(id, _)| id == policy_id)
            .count()
    }

    /// The snapshot `policy_id` holds of version `version`, if one was ever taken.
    pub fn policy_snapshot(&self, policy_id: &str, version: u32) -> Option<&PolicySnapshot> {
        self.policy_snapshots
            .get(&(policy_id.to_string(), version))
    }

    /// Every member a policy-shaped response carries for what `identifier` names: the live
    /// policy for the policy's id or ARN, and the snapshot of one version for that id or
    /// ARN carrying the version suffix a version-qualified identifier is written with —
    /// `None` when neither answers to it.
    pub fn policy_facts(&self, identifier: &str) -> Option<PolicyFacts> {
        if let Some(policy) = self.policy_by_identifier(identifier) {
            return Some(policy.facts());
        }
        let (unversioned, version) = version_qualified(identifier)?;
        let policy = self.policy_by_identifier(unversioned)?;
        let snapshot = self.policy_snapshot(&policy.policy_id, version)?;
        Some(snapshot.facts(policy))
    }
}

/// The policy identifier and the version a version-qualified identifier names — `None`
/// when the value carries no version suffix.
///
/// The suffix is read exactly as a reference's is ([`super::is_entity_version`]), so one
/// reading of "`<identifier>`:`<version>`" serves the reference graph and this lookup.
fn version_qualified(identifier: &str) -> Option<(&str, u32)> {
    let (unversioned, suffix) = identifier.rsplit_once(':')?;
    if !super::references::is_entity_version(suffix) {
        return None;
    }
    Some((unversioned, suffix.parse().ok()?))
}
