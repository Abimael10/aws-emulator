// SPDX-License-Identifier: CC0-1.0

//! The Rule entity: the record `CreateRule` persists and `GetRule` reads back.
//!
//! Kept beside [`super`]'s policy record rather than inside any one `src/ops/*.rs`, so
//! every rule operation agrees on one copy of the record, the id minting and the
//! `ruleIdentifier` resolution.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::{
    ComAmazonawsNetworksecuritymanagerPiccolocustomerapiserviceState as State, mint_entity_id,
    to_date_time,
};

/// `ServiceResourceType` naming a rule, for the `resourceType` member the modelled
/// `ConflictException` / `ResourceNotFoundException` / `ServiceQuotaExceededException`
/// shapes carry. One of `ServiceResourceType`'s modelled values.
pub const RESOURCE_TYPE_RULE: &str = "Rule";

/// The most rules this emulator stores, after which `CreateRule` answers with the
/// modelled `ServiceQuotaExceededException` (HTTP 402) instead of growing
/// [`State::rules`] for as long as a caller keeps asking. A CHOSEN emulator capacity,
/// not a quota this service documents: what matters is that the store has a ceiling and
/// that reaching it is refused through a modelled error rather than by exhausting the
/// host.
pub const MAX_RULES: usize = 100;

/// One persisted rule: every member `GetRule` answers with, plus the bookkeeping the
/// lifecycle needs (`tags`, `created_at`).
#[derive(Debug, Clone)]
pub struct Rule {
    pub rule_id: String,
    pub rule_arn: String,
    pub rule_name: String,
    pub rule_description: Option<String>,
    /// `RuleFirewallType`. Fixed at create: no modelled operation changes it.
    pub firewall_type: String,
    /// `RuleType` — `CONFIGURATION` or `INSPECTION`.
    pub rule_type: String,
    /// The `JsonDocument` the caller supplied, stored as given: the model declares it a
    /// document whose "structure depends on the rule's firewall type and rule type".
    pub configuration: ::aws_smithy_types::Document,
    /// `EntityStatus` — [`super::STATUS_ACTIVE`] or [`super::STATUS_DRAFT`], recomputed
    /// from `isPublished` on every create and update.
    pub status: String,
    /// Rendered as the modelled `EntityVersion` (`^[1-9][0-9]*$`) by
    /// [`Rule::version_string`]; starts at 1 and is bumped by every update.
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

impl Rule {
    /// The modelled `EntityVersion` rendering of [`Rule::version`].
    pub fn version_string(&self) -> String {
        self.version.to_string()
    }

    /// `updatedAt` as the modelled `DateTimestamp`.
    pub fn updated_at_timestamp(&self) -> ::aws_smithy_types::DateTime {
        to_date_time(self.updated_at)
    }
}

impl State {
    /// A fresh `RuleId`, minted exactly as a `PolicyId` is (see [`mint_entity_id`]): it
    /// satisfies the modelled `^[a-z0-9]{1,64}$` and cannot collide with an id this store
    /// already handed out.
    pub fn mint_rule_id(&mut self) -> String {
        mint_entity_id(&mut self.next_rule_seq)
    }

    /// The `ruleId` a `ruleIdentifier` names — the rule's ARN, as the model documents it,
    /// or the bare id — or `None` when no stored rule answers to it.
    pub fn resolve_rule_id(&self, identifier: &str) -> Option<String> {
        if self.rules.contains_key(identifier) {
            return Some(identifier.to_string());
        }
        self.rules
            .values()
            .find(|rule| rule.rule_arn == identifier)
            .map(|rule| rule.rule_id.clone())
    }

    /// The rule a `ruleIdentifier` names.
    pub fn rule_by_identifier(&self, identifier: &str) -> Option<&Rule> {
        let id = self.resolve_rule_id(identifier)?;
        self.rules.get(&id)
    }
}
