// SPDX-License-Identifier: CC0-1.0

//! A Rule's published version and the snapshots taken of it: the records
//! `CreateRuleSnapshot` persists and every rule-shaped response renders.
//!
//! Kept beside [`super`]'s live rule record rather than inside any one `src/ops/*.rs`, so
//! the live/frozen split a snapshot answers with ([`RuleFacts`]) is decided in one place
//! and every operation that can answer with a snapshot answers alike — exactly as
//! [`super::PolicySnapshot`] does for a policy.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{
    ComAmazonawsNetworksecuritymanagerPiccolocustomerapiserviceState as State, Rule, STATUS_ACTIVE,
    to_date_time, version_qualified,
};

/// The most snapshots this emulator keeps of ONE rule, after which `CreateRuleSnapshot`
/// answers with the modelled `ServiceQuotaExceededException` (HTTP 402) instead of growing
/// [`State::rule_snapshots`] for as long as a caller keeps asking. A CHOSEN emulator
/// capacity, not a quota this service documents: what matters is that the store has a
/// ceiling and that reaching it is refused through a modelled error rather than by
/// exhausting the host.
pub const MAX_RULE_SNAPSHOTS: usize = 100;

/// One version of a rule: the members an update writes as a unit (`ruleType`,
/// `ruleDescription`, `configuration`), frozen as they stood when that version was
/// published.
///
/// A snapshot is "an immutable, versioned copy" of the rule's published version, so the
/// published body has to be kept apart from the live record: a draft update moves the live
/// record on without touching what is published, and a snapshot taken afterwards still
/// copies the published body.
#[derive(Debug, Clone)]
pub struct RuleVersion {
    /// The `EntityVersion` the live record carried when this version was published.
    pub version: u32,
    pub rule_description: Option<String>,
    /// `RuleType` — one of the members `UpdateRule` writes, so it belongs to the version
    /// rather than to the rule as a whole.
    pub rule_type: String,
    pub configuration: ::aws_smithy_types::Document,
}

/// One snapshot of a rule: an immutable copy of the rule's published version at the moment
/// the snapshot was taken, plus the bookkeeping a snapshot of its own needs (`tags`, the
/// time it was created, its own `updateToken`).
#[derive(Debug, Clone)]
pub struct RuleSnapshot {
    /// The `ruleId` of the rule this is a snapshot of. A snapshot is a version of that
    /// rule, not a rule of its own, so it answers with the same id.
    pub rule_id: String,
    /// The snapshot's own `ruleArn`: the rule's ARN carrying the version suffix a
    /// version-qualified identifier of this service is written with (see
    /// [`super::is_entity_version`]), which is what lets a caller read this one version
    /// back while the live rule moves on.
    pub rule_arn: String,
    /// The published body this snapshot froze.
    pub published: RuleVersion,
    /// `UpdateToken` — minted once, with the snapshot, and never rotated: every read of an
    /// immutable copy returns the same token.
    pub update_token: String,
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
}

/// Every member a rule-shaped response carries, resolved for one rule or for one of its
/// snapshots.
///
/// The one home for the live/frozen split, so `CreateRuleSnapshot` and a read that
/// resolves a version-qualified identifier cannot disagree about what a snapshot holds.
#[derive(Debug, Clone)]
pub struct RuleFacts {
    pub rule_id: String,
    pub rule_arn: String,
    pub rule_name: String,
    pub rule_description: Option<String>,
    pub firewall_type: String,
    pub rule_type: String,
    pub configuration: ::aws_smithy_types::Document,
    pub status: String,
    pub version: String,
    pub update_token: String,
    pub is_snapshot: bool,
    pub has_published_version: bool,
    pub updated_at: ::aws_smithy_types::DateTime,
}

impl Rule {
    /// This rule's current body as one version: what publishing it records, and what a
    /// snapshot of the published version copies.
    pub fn version_body(&self) -> RuleVersion {
        RuleVersion {
            version: self.version,
            rule_description: self.rule_description.clone(),
            rule_type: self.rule_type.clone(),
            configuration: self.configuration.clone(),
        }
    }

    /// Every member a rule-shaped response carries, for the LIVE rule.
    pub fn facts(&self) -> RuleFacts {
        RuleFacts {
            rule_id: self.rule_id.clone(),
            rule_arn: self.rule_arn.clone(),
            rule_name: self.rule_name.clone(),
            rule_description: self.rule_description.clone(),
            firewall_type: self.firewall_type.clone(),
            rule_type: self.rule_type.clone(),
            configuration: self.configuration.clone(),
            status: self.status.clone(),
            version: self.version_string(),
            update_token: self.update_token.clone(),
            is_snapshot: self.is_snapshot,
            has_published_version: self.has_published_version,
            updated_at: self.updated_at_timestamp(),
        }
    }
}

impl RuleSnapshot {
    /// Every member a rule-shaped response carries, for this snapshot of `live`.
    ///
    /// What the published version holds comes from the frozen copy — the `configuration`,
    /// the `ruleType` and the `ruleDescription` that were copied, and the `version` they
    /// were copied at — so it stays as it stood however far the live rule moves on. What
    /// belongs to the rule as a whole rather than to one of its versions is read from the
    /// live record, so a snapshot reports the rule's current `ruleName` and
    /// `firewallType`. `updatedAt` is "The time when the snapshot was created" (the model,
    /// on this very member), and the copy is of a PUBLISHED version, so its status is
    /// [`STATUS_ACTIVE`].
    pub fn facts(&self, live: &Rule) -> RuleFacts {
        RuleFacts {
            rule_id: self.rule_id.clone(),
            rule_arn: self.rule_arn.clone(),
            rule_name: live.rule_name.clone(),
            rule_description: self.published.rule_description.clone(),
            firewall_type: live.firewall_type.clone(),
            rule_type: self.published.rule_type.clone(),
            configuration: self.published.configuration.clone(),
            status: STATUS_ACTIVE.to_string(),
            version: self.published.version.to_string(),
            update_token: self.update_token.clone(),
            is_snapshot: true,
            has_published_version: true,
            updated_at: to_date_time(self.created_at),
        }
    }
}

impl State {
    /// How many snapshots this store holds of `rule_id` — the count [`MAX_RULE_SNAPSHOTS`]
    /// bounds.
    pub fn rule_snapshot_count(&self, rule_id: &str) -> usize {
        self.rule_snapshots
            .keys()
            .filter(|(id, _)| id == rule_id)
            .count()
    }

    /// The snapshot `rule_id` holds of version `version`, if one was ever taken.
    pub fn rule_snapshot(&self, rule_id: &str, version: u32) -> Option<&RuleSnapshot> {
        self.rule_snapshots.get(&(rule_id.to_string(), version))
    }

    /// Every member a rule-shaped response carries for what `identifier` names: the live
    /// rule for the rule's ARN, and the snapshot of one version for that ARN carrying the
    /// version suffix a version-qualified identifier is written with — `None` when neither
    /// answers to it.
    pub fn rule_facts(&self, identifier: &str) -> Option<RuleFacts> {
        if let Some(rule) = self.rule_by_identifier(identifier) {
            return Some(rule.facts());
        }
        let (unversioned, version) = version_qualified(identifier)?;
        let rule = self.rule_by_identifier(unversioned)?;
        let snapshot = self.rule_snapshot(&rule.rule_id, version)?;
        Some(snapshot.facts(rule))
    }
}
