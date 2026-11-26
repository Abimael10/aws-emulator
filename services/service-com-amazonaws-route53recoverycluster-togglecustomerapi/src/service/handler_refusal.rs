// SPDX-License-Identifier: CC0-1.0

//! The modelled refusals this API's HANDLERS construct, each shaped and worded in ONE place.
//!
//! A refusal that more than one handler answers is one refusal of this API, so its modelled
//! class, its `Reason`, which members it reports in `Fields` and the words it carries are
//! decided here and nowhere else — the same way the invalid-ARN refusal is decided in
//! [`super::resource_arn::invalid_resource_arn`] and the sentences themselves in
//! [`crate::state`]. Each constructor answers the SHARED `crate::ops::types` structure, and
//! a handler only lifts it into its own generated error enum
//! (`…ErrorModeled::ValidationException`, `…ErrorModeled::ResourceNotFoundException`), which
//! is the one part of the answer that is per operation.
//!
//! Three refusals of this API are reached from more than one handler, so they live here:
//!
//! * [`safety_rule_violation`] — both state updates;
//! * [`routing_control_not_found`] — the state read and both state updates;
//! * [`routing_control_state_outside_value_set`] — both state updates.
//!
//! A refusal only ONE operation can answer (the batch quota, a duplicate entry, a batch
//! spanning two clusters) stays with that operation, where nothing can disagree with it.

use crate::ops::types::{
    ResourceNotFoundException, ValidationException, ValidationExceptionField,
    ROUTING_CONTROL_STATE_VALUES,
};
use crate::state::SafetyRuleViolation;

/// The modelled `ValidationExceptionReason` a refusal that is about no single member of the
/// request carries: the request's shape is valid and something else about it is not
/// (`smithy_query(query="shape", shape="ValidationExceptionReason", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`).
const REASON_OTHER: &str = "other";

/// The modelled `ValidationExceptionReason` a refusal of ONE member's value carries — the
/// refusal that also names that member in `Fields`.
const REASON_FIELD_VALIDATION_FAILED: &str = "fieldValidationFailed";

/// The refusal a deployed safety rule answers a routing-control state update with.
///
/// It is about no single member of the request — the request is well formed and the control
/// panel's standing configuration is what refuses it — so it carries `Reason` `other` and
/// names no member in `Fields`, and its sentence is the violation's own
/// ([`crate::state::DataPlane::safety_rule_violation`]). Both state updates state it from
/// here, so a rule that blocks a single update and a rule that blocks a batch are the same
/// answer.
#[must_use]
pub fn safety_rule_violation(violation: SafetyRuleViolation) -> ValidationException {
    ValidationException {
        message: violation.message,
        reason: Some(REASON_OTHER.to_string()),
        fields: None,
    }
}

/// The refusal a `RoutingControlArn` the control plane minted no routing control for is
/// answered with.
///
/// The ARN the request carried is the resource that was not found, the kind of resource
/// looked up is the one every `RoutingControlArn` addresses
/// ([`crate::state::ROUTING_CONTROL_RESOURCE_TYPE`]), and the sentence is this service's one
/// wording for it ([`crate::state::routing_control_not_found_message`]). The state read and
/// both state updates state it from here.
#[must_use]
pub fn routing_control_not_found(routing_control_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: crate::state::routing_control_not_found_message(routing_control_arn),
        resource_id: routing_control_arn.to_string(),
        resource_type: crate::state::ROUTING_CONTROL_RESOURCE_TYPE.to_string(),
    }
}

/// The refusal a `RoutingControlState` outside the modelled value set is answered with,
/// `member_path` being where in the request that value sat — the input's own member, or the
/// member of a batch entry.
///
/// One member's value is what failed, so the refusal carries `Reason`
/// `fieldValidationFailed` and names that member in `Fields` with the requirement the value
/// did not satisfy: the modelled value set, in the order the model declares it
/// ([`ROUTING_CONTROL_STATE_VALUES`]). The requirement clause is the member-independent one
/// a constraint report states, so WHICH member carried the value changes the member path and
/// nothing else, and both state updates cannot word one requirement two ways.
#[must_use]
pub fn routing_control_state_outside_value_set(member_path: &str) -> ValidationException {
    let value_set = ROUTING_CONTROL_STATE_VALUES.join(", ");
    ValidationException {
        message: format!("RoutingControlState must satisfy enum value set: [{value_set}]"),
        reason: Some(REASON_FIELD_VALIDATION_FAILED.to_string()),
        fields: Some(vec![ValidationExceptionField {
            name: member_path.to_string(),
            message: format!("Member must satisfy enum value set: [{value_set}]"),
        }]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a refusal several handlers answer has ONE shape and ONE wording, so the only
    /// thing that varies between two callers of a constructor is what the request carried.
    #[test]
    fn the_enum_refusal_states_one_requirement_whatever_member_carried_the_value() {
        // (case, the member path a handler reports) — the two places this model carries a
        // `RoutingControlState`: the single update's own member and a batch entry's.
        let rows = [
            ("the input's own member", "RoutingControlState"),
            (
                "a batch entry's member",
                "UpdateRoutingControlStateEntries.RoutingControlState",
            ),
        ];
        let expected_message = "RoutingControlState must satisfy enum value set: [Off, On]";
        let expected_requirement = "Member must satisfy enum value set: [Off, On]";
        for (case, member_path) in rows {
            let refusal = routing_control_state_outside_value_set(member_path);
            assert_eq!(
                refusal.message, expected_message,
                "case {case:?}: expected message {expected_message:?}, got {:?}",
                refusal.message
            );
            assert_eq!(
                refusal.reason.as_deref(),
                Some(REASON_FIELD_VALIDATION_FAILED),
                "case {case:?}: expected reason {REASON_FIELD_VALIDATION_FAILED:?}, got {:?}",
                refusal.reason
            );
            let fields = refusal
                .fields
                .unwrap_or_else(|| panic!("case {case:?}: the refusal names the member"));
            assert_eq!(
                fields.len(),
                1,
                "case {case:?}: expected one named member, got {fields:?}"
            );
            assert_eq!(
                (fields[0].name.as_str(), fields[0].message.as_str()),
                (member_path, expected_requirement),
                "case {case:?}: expected the member {member_path:?} with requirement \
                 {expected_requirement:?}, got {:?}",
                fields[0]
            );
        }
    }

    /// RULE: the not-found refusal reports the ARN the request carried as the resource that
    /// was not found, the kind of resource a `RoutingControlArn` addresses, and this
    /// service's one sentence for it — for every ARN, whichever handler looked it up.
    #[test]
    fn the_not_found_refusal_reports_the_arn_the_request_carried() {
        // (case, the ARN a request carried)
        let rows = [
            (
                "a routing control ARN naming nothing",
                "arn:aws:route53-recovery-control::000000000000:controlpanel/p0/routingcontrol/r0",
            ),
            (
                "another partition's",
                "arn:aws-us-gov:route53-recovery-control::000000000000:controlpanel/p1/routingcontrol/r1",
            ),
            (
                "one carrying a Region",
                "arn:aws:route53-recovery-control:us-east-1:000000000000:controlpanel/p2/routingcontrol/r2",
            ),
        ];
        for (case, arn) in rows {
            let refusal = routing_control_not_found(arn);
            let expected_message = format!("Routing Control '{arn}' not found");
            assert_eq!(
                refusal.message, expected_message,
                "case {case:?}: expected message {expected_message:?}, got {:?}",
                refusal.message
            );
            assert_eq!(
                (refusal.resource_id.as_str(), refusal.resource_type.as_str()),
                (arn, crate::state::ROUTING_CONTROL_RESOURCE_TYPE),
                "case {case:?}: expected the ARN and the routing control resource type, got \
                 {refusal:?}"
            );
        }
    }

    /// RULE: the safety-rule refusal carries the violation's own sentence, is about no
    /// member of the request, and so names none.
    #[test]
    fn the_safety_rule_refusal_carries_the_violation_sentence_and_names_no_member() {
        // (case, the violation a rule evaluation reported)
        let rows = [
            ("an assertion rule", "rule-a", "At least 2 routing controls must be On"),
            ("a gating rule", "rule-g", "Fewer than 1 routing controls must be On"),
        ];
        for (case, rule, sentence) in rows {
            let refusal = safety_rule_violation(SafetyRuleViolation {
                safety_rule_arn: rule.to_string(),
                message: sentence.to_string(),
            });
            assert_eq!(
                refusal.message, sentence,
                "case {case:?}: expected the violation's sentence {sentence:?}, got {:?}",
                refusal.message
            );
            assert_eq!(
                refusal.reason.as_deref(),
                Some(REASON_OTHER),
                "case {case:?}: expected reason {REASON_OTHER:?}, got {:?}",
                refusal.reason
            );
            assert!(
                refusal.fields.is_none(),
                "case {case:?}: the refusal names no member, got {:?}",
                refusal.fields
            );
        }
    }
}
