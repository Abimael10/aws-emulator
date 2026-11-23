// SPDX-License-Identifier: CC0-1.0

//! One gateway's rules: the record a rule is stored as, the clock-owed status it
//! reports, and the response envelopes every operation that reports one renders.
//!
//! One concern, kept beside the gateway record that holds the rules rather than
//! in any one `ops/` file: a rule is addressed only THROUGH its gateway
//! (`/gateways/{gatewayIdentifier}/rules/{ruleId}`), so the create and every
//! later read answer from this one shape and cannot drift.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use crate::ops::types::{
    Action, Condition, ConflictException, CreateGatewayRuleOutput, GatewayRuleDetail,
    GetGatewayRuleOutput, ResourceNotFoundException, SystemManagedBlock, UpdateGatewayRuleOutput,
    ValidationException,
};

use super::{
    status_in_prose, timestamp, GatewayRecord, STATUS_READY, VALIDATION_REASON_RESOURCE_CONFLICT,
};

/// `GatewayRuleStatus` members this service drives. The enum's full value list
/// is `CREATING | ACTIVE | UPDATING | DELETING`
/// (`smithy_query shape=GatewayRuleStatus`).
pub const GATEWAY_RULE_STATUS_CREATING: &str = "CREATING";
pub const GATEWAY_RULE_STATUS_ACTIVE: &str = "ACTIVE";
pub const GATEWAY_RULE_STATUS_UPDATING: &str = "UPDATING";

/// How long a freshly created rule stays in its transient status (`CREATING`)
/// before the clock owes it `ACTIVE`.
///
/// A rule is a child of the gateway that evaluates it, so it settles on the same
/// span the gateway family uses ([`super::GATEWAY_SETTLE_DELAY`]): one chosen
/// span for the whole family rather than a second one only this resource would
/// wait out.
pub const GATEWAY_RULE_SETTLE_DELAY: Duration = super::GATEWAY_SETTLE_DELAY;

/// How this API spells the gateway-rule resource TYPE in the refusals that name
/// it, which is the modelled shape's own name and not the lower-case noun the
/// gateway and target families are reported missing by
/// ([`super::resource_not_found`]).
pub const RESOURCE_GATEWAY_RULE: &str = "GatewayRule";

/// The modelled not-found error for one RULE a gateway does not hold: the
/// resource type, the `ruleId` that was asked for, and that it is not there
/// ("GatewayRule 7f9b2c41-... not found").
///
/// The rule family reports a missing rule in its OWN sentence rather than
/// through [`super::resource_not_found`]: the sentence names the rule the
/// request addressed, so a caller that fans a request out over several rule ids
/// can attribute the failure to one of them. The text around the id is fixed and
/// `rule_id` is its only variable part — the value the request's `ruleId` path
/// label carried.
pub fn gateway_rule_not_found(rule_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("{RESOURCE_GATEWAY_RULE} {rule_id} not found")),
    }
}

/// The modelled validation error a write of a gateway's RULES gets while the
/// gateway that would evaluate them has not reached its terminal status: the
/// gateway the request named, the status a rule write needs, and the status the
/// gateway is in ("Gateway my-gateway-abcdefghij is not in READY state. Current
/// status: Creating").
///
/// The sentence is fixed; its variable parts are the identifier the request's
/// path carried and the gateway's current status, spelled as prose
/// ([`super::status_in_prose`]) rather than as the wire's enum token. The rule
/// family words this refusal in its own sentence — neither the one an operation
/// on the GATEWAY itself is refused in ([`super::gateway_status_conflict`]) nor
/// the one an operation on its TARGETS is
/// ([`super::gateway_target_status_conflict`]).
///
/// `reason` is [`super::VALIDATION_REASON_RESOURCE_CONFLICT`] and there is no
/// `fieldList`: nothing is wrong with the request's members, it is the gateway's
/// state that refuses it.
pub fn gateway_not_ready_for_rule(gateway_identifier: &str, status: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "Gateway {gateway_identifier} is not in {STATUS_READY} state. Current status: {}",
            status_in_prose(status)
        ),
        reason: VALIDATION_REASON_RESOURCE_CONFLICT.to_string(),
        field_list: None,
    }
}

/// The refusal a write of one gateway's RULES owes when the gateway it addresses
/// cannot take it, or `None` when it can.
///
/// A rule decides how the gateway routes and processes the requests it fronts, so
/// it is accepted only once that gateway is routable — once the clock has moved
/// it to [`STATUS_READY`]. Every other lifecycle status is refused: the transient
/// one the clock still owes a transition out of, and the one a gateway on its way
/// out reports. The caller is told which status it found and waits that status
/// out, which is the ordering a client that creates a gateway and then configures
/// it follows.
///
/// `gateway_identifier` is the identifier the request's path carried, which the
/// refusal names. Shared by every write of a gateway's rules, so they cannot put
/// different preconditions on the same gateway.
pub fn refusal_for_gateway_rule_write(
    gateway_identifier: &str,
    gateway: &GatewayRecord,
    now: SystemTime,
) -> Option<ValidationException> {
    let status = gateway.status_at(now);
    (status != STATUS_READY).then(|| gateway_not_ready_for_rule(gateway_identifier, status))
}

/// The modelled validation error a write of a gateway's rules gets when the
/// `priority` it asks for is one a rule of that same gateway already holds
/// ("Priority 500 conflicts with an existing rule").
///
/// The sentence is fixed and the priority is its only variable part — the value
/// the request's `priority` member carried, rendered as the integer it is.
/// `reason` is [`super::VALIDATION_REASON_RESOURCE_CONFLICT`] and there is no
/// `fieldList`: the member itself satisfies every modelled constraint, it is the
/// rule already holding that priority that refuses it.
pub fn gateway_rule_priority_conflict(priority: i32) -> ValidationException {
    ValidationException {
        message: format!("Priority {priority} conflicts with an existing rule"),
        reason: VALIDATION_REASON_RESOURCE_CONFLICT.to_string(),
        field_list: None,
    }
}

/// The rule this gateway already holds at `priority`, if one does.
///
/// "Rules are evaluated in order of priority, with lower numbers evaluated
/// first" (`smithy_query doc shape=CreateGatewayRuleRequest member=priority`):
/// the priority is the gateway's rule-EVALUATION key, so one gateway holds at
/// most one rule at any priority — two rules at one priority would leave which
/// of them is evaluated there undecidable. The scope is the one gateway: the
/// rules of a DIFFERENT gateway are evaluated in their own order, so the same
/// priority is free on each.
///
/// Shared by every write of a gateway's rules, so they cannot disagree about
/// which priorities one gateway's rule set can hold.
pub fn rule_holding_priority(
    rules: &BTreeMap<String, GatewayRuleRecord>,
    priority: i32,
) -> Option<&GatewayRuleRecord> {
    rule_holding_priority_other_than(rules, priority, None)
}

/// The rule OTHER THAN `except` this gateway already holds at `priority`, if one
/// does.
///
/// The same evaluation-order rule [`rule_holding_priority`] decides, for a write
/// that reconfigures a rule the gateway already holds: a rule keeping — or
/// repeating — its own priority collides with nothing, because it is the one rule
/// evaluated there, so only a SIBLING holding that priority refuses the change.
/// `except` is the `ruleId` the request addressed, and `None` is a write that
/// reconfigures no stored rule.
pub fn rule_holding_priority_other_than<'rules>(
    rules: &'rules BTreeMap<String, GatewayRuleRecord>,
    priority: i32,
    except: Option<&str>,
) -> Option<&'rules GatewayRuleRecord> {
    rules
        .values()
        .find(|held| held.priority == priority && except.is_none_or(|id| held.rule_id != id))
}

/// The modelled conflict a change asked of one gateway rule owes while the rule's
/// own write is still under way: its create, or a previous change, has not taken
/// effect yet, so this one would overwrite a version that is still being applied.
///
/// The sentence has no variable part: it names the collision and what the caller
/// does about it rather than the rule it was asked of, because an optimistic lock
/// is held by the write in flight and not by anything the request could name. It
/// is the wording this API gives that error class, so a client that matches on it
/// reads the same text the real service sends.
pub fn gateway_rule_lock_conflict() -> ConflictException {
    ConflictException {
        message: Some(GATEWAY_RULE_LOCK_CONFLICT.to_string()),
    }
}

/// What a write that loses to a change already being applied is told: that the
/// resource is locked by a concurrent write, and to retry once that write has
/// finished.
const GATEWAY_RULE_LOCK_CONFLICT: &str =
    "Failed to update the resource due to an existing lock. This means that there was a \
     concurrent request that updated the resource. Retry the request after the other request \
     has completed.";

/// The refusal a change of one STORED rule owes while that rule is still settling,
/// or `None` when the rule can take it.
///
/// A rule's status is the state of the write that last touched it, and the
/// transition out of a transient one is owed by the clock: while that transition
/// is still owed, the write that set it is still being applied, so a further
/// change to the same rule would race it. That is the optimistic lock the real
/// service reports with the modelled `ConflictException` (HTTP 409), and the rule
/// keeps the configuration it has. Only a rule the clock has settled into
/// [`GATEWAY_RULE_STATUS_ACTIVE`] takes one.
///
/// Shared by every write that reconfigures a stored rule, so they cannot put
/// different preconditions on the same rule.
pub fn refusal_for_gateway_rule_change(
    rule: &GatewayRuleRecord,
    now: SystemTime,
) -> Option<ConflictException> {
    (rule.status_at(now) != GATEWAY_RULE_STATUS_ACTIVE).then(gateway_rule_lock_conflict)
}

/// How many rules one gateway will hold.
///
/// `CreateGatewayRule` models `ServiceQuotaExceededException` (HTTP 402) for
/// exceeding the real service's per-gateway quota. The number is NOT a fidelity
/// claim about that quota: it is the emulator's own ceiling, set generously so
/// no realistic workflow meets it, so that a caller looping on create cannot
/// grow this process's memory without bound.
pub const MAX_RULES_PER_GATEWAY: usize = 1_000;

/// One rule configured on a gateway: what the caller submitted for it, and the
/// transition to `ACTIVE` the clock owes it.
///
/// Every operation that reports a rule renders it from this record, so a create
/// and a later read cannot disagree about its priority, its conditions, its
/// actions or its status.
#[derive(Debug, Clone)]
pub struct GatewayRuleRecord {
    pub rule_id: String,
    pub priority: i32,
    /// The conditions the caller submitted, absent for a rule that matches every
    /// request — `conditions` is optional on the create and optional on every
    /// response, so a rule created without any reports none.
    pub conditions: Option<Vec<Condition>>,
    pub actions: Vec<Action>,
    pub description: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The status until [`settles_at`](Self::settles_at) passes.
    pub transient_status: String,
    /// The status from [`settles_at`](Self::settles_at) onwards.
    pub settled_status: String,
    /// The instant the transient status is owed its transition. Stored, never
    /// slept on: a read materialises the transition the clock already made due.
    pub settles_at: SystemTime,
    /// The system or process that manages this rule, for a rule an automated
    /// process created; `None` for one a caller created.
    ///
    /// `system` is "System-managed metadata for rules created by automated
    /// processes" (`smithy_query doc shape=CreateGatewayRuleResponse
    /// member=system`), and no operation this service models lets a caller
    /// supply it, so a rule a caller created carries none.
    pub managed_by: Option<String>,
}

impl GatewayRuleRecord {
    /// The record a create stores for one submitted rule, under the id the
    /// create minted for it.
    pub fn written(
        rule_id: String,
        priority: i32,
        conditions: Option<Vec<Condition>>,
        actions: Vec<Action>,
        description: Option<String>,
        now: SystemTime,
    ) -> Self {
        Self {
            rule_id,
            priority,
            conditions,
            actions,
            description,
            created_at: now,
            updated_at: now,
            // A new rule is created, not active: the transition to ACTIVE is
            // owed by the clock, not by this call.
            transient_status: GATEWAY_RULE_STATUS_CREATING.to_string(),
            settled_status: GATEWAY_RULE_STATUS_ACTIVE.to_string(),
            settles_at: now + GATEWAY_RULE_SETTLE_DELAY,
            // Nothing but an automated process manages a rule, and no operation
            // this service models lets a caller claim one.
            managed_by: None,
        }
    }

    /// The status this rule reports at `now`: the transient one until the
    /// transition the clock owes comes due, the settled one afterwards.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// Apply one submitted change to this stored rule.
    ///
    /// `UpdateGatewayRule` "updates a gateway rule's priority, conditions,
    /// actions, or description" over `PATCH`, and every one of those four members
    /// is optional on the request while two of them are `@required` on its
    /// response (`smithy_query operation=UpdateGatewayRule`): a member the request
    /// CARRIES replaces what the rule held, and one it omits leaves that part of
    /// the rule as it was. So a request carrying one member changes that one
    /// thing, and the response reports the rule as it now stands rather than a
    /// default this call invented for a member nobody submitted.
    ///
    /// The id the rule is addressed by and the instant it was created are not
    /// touched. The change is not in effect at once: the rule reports
    /// [`GATEWAY_RULE_STATUS_UPDATING`] until the clock owes it
    /// [`GATEWAY_RULE_STATUS_ACTIVE`] again, exactly as a created one reports
    /// `CREATING`.
    pub fn apply_change(
        &mut self,
        priority: Option<i32>,
        conditions: Option<Vec<Condition>>,
        actions: Option<Vec<Action>>,
        description: Option<String>,
        now: SystemTime,
    ) {
        if let Some(priority) = priority {
            self.priority = priority;
        }
        if let Some(conditions) = conditions {
            self.conditions = Some(conditions);
        }
        if let Some(actions) = actions {
            self.actions = actions;
        }
        if let Some(description) = description {
            self.description = Some(description);
        }
        self.updated_at = now;
        self.transient_status = GATEWAY_RULE_STATUS_UPDATING.to_string();
        self.settled_status = GATEWAY_RULE_STATUS_ACTIVE.to_string();
        self.settles_at = now + GATEWAY_RULE_SETTLE_DELAY;
    }

    /// The `system` block this rule reports, for a rule an automated process
    /// manages, and `None` for one a caller created.
    pub fn system(&self) -> Option<SystemManagedBlock> {
        self.managed_by
            .as_ref()
            .map(|managed_by| SystemManagedBlock {
                managed_by: managed_by.clone(),
            })
    }

    /// This rule as `CreateGatewayRule`'s response envelope. `gateway_arn` is the
    /// owning gateway's, which the response reports and the rule itself does not
    /// store twice.
    pub fn create_output(&self, gateway_arn: &str, now: SystemTime) -> CreateGatewayRuleOutput {
        CreateGatewayRuleOutput {
            rule_id: self.rule_id.clone(),
            gateway_arn: gateway_arn.to_string(),
            priority: self.priority,
            conditions: self.conditions.clone(),
            actions: self.actions.clone(),
            description: self.description.clone(),
            created_at: timestamp(self.created_at),
            status: self.status_at(now).to_string(),
            system: self.system(),
        }
    }

    /// This rule as one entry of a LISTING of its gateway's rules — the same
    /// members [`Self::get_output`] reports, off the same record, so a rule read
    /// one at a time and the same rule read out of the list cannot disagree.
    pub fn detail(&self, gateway_arn: &str, now: SystemTime) -> GatewayRuleDetail {
        let read = self.get_output(gateway_arn, now);
        GatewayRuleDetail {
            rule_id: read.rule_id,
            gateway_arn: read.gateway_arn,
            priority: read.priority,
            conditions: read.conditions,
            actions: read.actions,
            description: read.description,
            created_at: read.created_at,
            status: read.status,
            system: read.system,
            updated_at: read.updated_at,
        }
    }

    /// This rule as `GetGatewayRule`'s response envelope — the same members
    /// [`Self::create_output`] renders, read off the same record, plus the
    /// `updatedAt` only a read declares, so a read cannot differ from the create
    /// that stored it.
    pub fn get_output(&self, gateway_arn: &str, now: SystemTime) -> GetGatewayRuleOutput {
        let created = self.create_output(gateway_arn, now);
        GetGatewayRuleOutput {
            rule_id: created.rule_id,
            gateway_arn: created.gateway_arn,
            priority: created.priority,
            conditions: created.conditions,
            actions: created.actions,
            description: created.description,
            created_at: created.created_at,
            status: created.status,
            system: created.system,
            updated_at: Some(timestamp(self.updated_at)),
        }
    }

    /// This rule as `UpdateGatewayRule`'s response envelope — the same members a
    /// read of it reports, off the same record, so the change this call made is
    /// reported exactly as the next read of the rule reports it.
    pub fn update_output(&self, gateway_arn: &str, now: SystemTime) -> UpdateGatewayRuleOutput {
        let read = self.get_output(gateway_arn, now);
        UpdateGatewayRuleOutput {
            rule_id: read.rule_id,
            gateway_arn: read.gateway_arn,
            priority: read.priority,
            conditions: read.conditions,
            actions: read.actions,
            description: read.description,
            created_at: read.created_at,
            status: read.status,
            system: read.system,
            updated_at: read.updated_at,
        }
    }
}
