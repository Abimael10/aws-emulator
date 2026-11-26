// SPDX-License-Identifier: CC0-1.0

//! The `/responder-gateway/{gatewayId}/link/{linkId}/routing-rule` surface's state:
//! the routing rules a stored link carries.
//!
//! A rule belongs to one link, so it is keyed by (`linkId`, `ruleId`) and released
//! with that link ([`release_link`](super::ComAmazonawsRtbfabricRtbfabricState::release_link)):
//! no read can report a rule whose link is gone.
//!
//! A rule's configuration — the priority it is evaluated at and the conditions it
//! matches on — is changed by `UpdateLinkRoutingRule`, which the service answers
//! before the change is routing: a rule that was routing holds
//! `UPDATE_IN_PROGRESS` until the clock makes `ACTIVE` due again.
//!
//! A rule of its own is released by `DeleteLinkRoutingRule`, which the service
//! answers before the rule is gone: the record holds `DELETION_IN_PROGRESS` until
//! the clock makes the deletion due, and
//! [`settle_all`](super::ComAmazonawsRtbfabricRtbfabricState::settle_all) then drops
//! it, so no later read reports it.

use std::collections::HashMap;
use std::time::SystemTime;

use super::ComAmazonawsRtbfabricRtbfabricState;
use crate::ops::types::RuleCondition;

/// `RuleStatus` member: the rule is accepted, it is not routing yet.
pub const RULE_STATUS_CREATION_IN_PROGRESS: &str = "CREATION_IN_PROGRESS";
/// `RuleStatus` member: the rule routes traffic.
pub const RULE_STATUS_ACTIVE: &str = "ACTIVE";
/// `RuleStatus` member: a change of the rule's configuration is accepted, the rule
/// is not routing on it yet.
pub const RULE_STATUS_UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";
/// `RuleStatus` member: the rule's deletion is accepted, it is not gone yet.
pub const RULE_STATUS_DELETION_IN_PROGRESS: &str = "DELETION_IN_PROGRESS";
/// `RuleStatus` member: terminal, the rule is gone.
pub const RULE_STATUS_DELETED: &str = "DELETED";

/// What a `conditions` structure carrying no condition at all is refused with.
///
/// "At least one condition field must be set" is the modelled documentation of
/// `RuleCondition` (`smithy_query(query="shape", shape="RuleCondition")`), and no
/// single member is `@required`, so the schema layer cannot refuse an empty structure
/// and the rule is stated here.
pub const RULE_CONDITIONS_EMPTY_MESSAGE: &str =
    "conditions must set at least one condition for the rule to match on";

/// How many routing rules the store holds before `CreateLinkRoutingRule` refuses with
/// the modelled `ServiceQuotaExceededException` (HTTP 402).
///
/// The emulator's own bound, for the same reason as
/// [`LINK_QUOTA`](super::LINK_QUOTA): a rule is released with its link, so this bounds
/// what is live rather than what was ever created.
pub const ROUTING_RULE_QUOTA: usize = 5_000;

/// A routing rule on a link, as `CreateLinkRoutingRule` persisted it.
#[derive(Debug, Clone)]
pub struct LinkRoutingRule {
    pub rule_id: String,
    /// The link the rule routes for; the rule is addressable from either of that
    /// link's two ends, exactly as the link itself is.
    pub link_id: String,
    pub priority: i32,
    pub conditions: RuleCondition,
    pub status: String,
    /// The instant the clock makes this rule's outstanding transition due — the
    /// one [`settles_to`](LinkRoutingRule::settles_to) names; `None` once the
    /// record has settled.
    pub settles_at: Option<SystemTime>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl LinkRoutingRule {
    /// The status the clock owes this rule next: a rule whose creation was
    /// accepted routes traffic, and one whose deletion was accepted is gone.
    /// `None` once the rule holds a status nothing moves it out of.
    pub fn settles_to(&self) -> Option<&'static str> {
        match self.status.as_str() {
            RULE_STATUS_CREATION_IN_PROGRESS | RULE_STATUS_UPDATE_IN_PROGRESS => {
                Some(RULE_STATUS_ACTIVE)
            }
            RULE_STATUS_DELETION_IN_PROGRESS => Some(RULE_STATUS_DELETED),
            _ => None,
        }
    }

    /// Whether this rule still has a configuration a call can change: it is
    /// routing, it is coming up, or it is already applying a change. A rule whose
    /// deletion is outstanding, one the clock has made gone and one that failed
    /// are each moving through — or stopped at — a lifecycle of their own, and
    /// hold nothing left to configure.
    pub fn configurable(&self) -> bool {
        matches!(
            self.status.as_str(),
            RULE_STATUS_CREATION_IN_PROGRESS | RULE_STATUS_ACTIVE | RULE_STATUS_UPDATE_IN_PROGRESS
        )
    }

    /// Take the configuration change the caller asked for: from here on the rule
    /// matches on the stated conditions at the stated priority.
    ///
    /// A rule that is routing takes the change live, so it reports
    /// `UPDATE_IN_PROGRESS` until the clock makes `ACTIVE` due again and a caller
    /// reading straight after the call still sees the transient status. A rule that
    /// has not finished coming up, or that is already applying a change, keeps the
    /// status it stands in and the instant its outstanding transition comes due:
    /// that transition ends where this one would, so a further change never pushes
    /// it back.
    pub fn take_update(&mut self, priority: i32, conditions: RuleCondition, now: SystemTime) {
        self.priority = priority;
        self.conditions = conditions;
        self.updated_at = now;
        if self.status == RULE_STATUS_ACTIVE {
            self.status = RULE_STATUS_UPDATE_IN_PROGRESS.to_string();
            self.settles_at = Some(now + super::PROVISIONING_DELAY);
        }
    }

    /// The message this service's `ConflictException` carries when this rule's
    /// current status does not admit the transition a call asks for: the rule and
    /// the link it routes for, and the status that refuses it, all from the record
    /// — worded exactly as [`super::Link::status_conflict_message`] words a link's.
    pub fn status_conflict_message(&self, transition: &str) -> String {
        format!(
            "rule {} on link {} is {} and cannot be {}",
            self.rule_id, self.link_id, self.status, transition
        )
    }

    /// Whether this rule's deletion is already outstanding, so a repeat of the
    /// delete asks for the state the rule is in rather than for a new one.
    pub fn deleting(&self) -> bool {
        self.status == RULE_STATUS_DELETION_IN_PROGRESS
    }

    /// Take the deletion the caller asked for: the rule stops routing once the
    /// clock makes `DELETED` due, so a caller reading straight after the call
    /// still observes the transient status.
    pub fn begin_deletion(&mut self, now: SystemTime) {
        self.status = RULE_STATUS_DELETION_IN_PROGRESS.to_string();
        self.settles_at = Some(now + super::PROVISIONING_DELAY);
        self.updated_at = now;
    }

    /// Materialise the transition the clock has already made due, dated at the
    /// instant it was owed rather than now.
    pub fn settle(&mut self, now: SystemTime) {
        let Some(due) = self.settles_at.filter(|due| *due <= now) else {
            return;
        };
        // Cleared whatever the status is, so the field's invariant — a due instant
        // only while one is owed — holds for a record in any status.
        self.settles_at = None;
        let Some(next) = self.settles_to() else {
            return;
        };
        self.status = next.to_string();
        self.updated_at = due;
    }
}

/// Whether `conditions` states anything for a request to match on.
///
/// Every member of the modelled `RuleCondition` is optional, so an empty structure
/// deserialises happily and would match every request; the shape's own documentation
/// is that at least one of them is set.
pub fn conditions_stated(conditions: &RuleCondition) -> bool {
    conditions.host_header.is_some()
        || conditions.host_header_wildcard.is_some()
        || conditions.path_prefix.is_some()
        || conditions.path_exact.is_some()
        || conditions.query_string_equals.is_some()
        || conditions.query_string_exists.is_some()
}

impl ComAmazonawsRtbfabricRtbfabricState {
    /// The link's rule that already holds `priority`, if one does.
    ///
    /// A priority orders the link's rules against each other — lower number
    /// evaluated first — so two rules of one link holding the same number leave
    /// that order undecided; the modelled `RulePriority`
    /// (`smithy_query(query="doc", shape="RulePriority")`) states the number is
    /// unique per link among the rules that are not deleted. A rule whose
    /// deletion is still outstanding has not been deleted, so it holds its
    /// priority until the clock completes that deletion and
    /// [`settle_all`](Self::settle_all) drops the record; the terminal status is
    /// excluded here anyway, so a record kept past it constrains nothing.
    ///
    /// Another link's rules are no constraint: the scan covers the rules keyed
    /// to THIS link, and so is bounded by [`ROUTING_RULE_QUOTA`] like every
    /// other scan this store does under its lock.
    pub fn rule_holding_priority(&self, link_id: &str, priority: i32) -> Option<&LinkRoutingRule> {
        // No rule id is empty — the modelled `RuleId` pattern requires a prefix —
        // so this excludes nothing.
        self.rule_holding_priority_other_than(link_id, "", priority)
    }

    /// [`rule_holding_priority`](Self::rule_holding_priority), less the rule
    /// `rule_id` itself.
    ///
    /// A call that moves ONE rule reads the number it asks for against the link's
    /// OTHER rules: the number a rule already holds is its own, so re-stating it is
    /// no conflict, while a number another non-deleted rule of the link holds is
    /// the same conflict whichever call asks for it.
    pub fn rule_holding_priority_other_than(
        &self,
        link_id: &str,
        rule_id: &str,
        priority: i32,
    ) -> Option<&LinkRoutingRule> {
        self.routing_rules.values().find(|rule| {
            rule.link_id == link_id
                && rule.rule_id != rule_id
                && rule.status != RULE_STATUS_DELETED
                && rule.priority == priority
        })
    }
}

/// What a call asking for a priority another non-deleted rule of the same link
/// already holds is refused with, beside the modelled `ConflictException` (HTTP
/// 409): the rule that holds the number, so the caller knows which one to read,
/// move or delete.
pub fn priority_conflict_message(rule_id: &str, link_id: &str, priority: i32) -> String {
    format!("rule {rule_id} on link {link_id} already holds priority {priority}")
}
