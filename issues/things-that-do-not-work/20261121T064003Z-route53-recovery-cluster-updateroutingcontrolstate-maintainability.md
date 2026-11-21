Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 18a314cdb2529802996db00768c0e97bdee3f604

# Hold the modelled-refusal construction in one place: the two update handlers duplicate the safety-rule→ValidationException and not-found→ResourceNotFoundException mappings, and the copies have already drifted

_Filed: 2026-11-21T06:40:03Z_
_Operation: UpdateRoutingControlState | Class: maintainability_

> _Note: this issue was identified against the code base at commit `18a314cdb2529802996db00768c0e97bdee3f604`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate already has the idiom for holding a refusal's SHAPE in one place: `crate::service::resource_arn::invalid_resource_arn()` returns the shared `ValidationException` struct and each handler only maps it into its own generated error enum (`.map_err(…ErrorModeled::ValidationException)`). The messages are centralised the same way, in `crate::state` (`routing_control_not_found_message`, `safety_rule_violation_message`, `batch_quota_exceeded_message`).

Three other refusals are NOT held that way — each is constructed inline, twice or three times, in sibling handlers of this crate:

1. the safety-rule refusal (which modelled class, `reason: Some("other")`, `fields: None`) — written identically in `update_routing_control_state.rs` and `update_routing_control_states.rs`;
2. the routing-control not-found refusal (message helper + `resource_id` = the ARN + `resource_type` = `ROUTING_CONTROL_RESOURCE_TYPE`) — written three times: a local `not_found()` helper in `update_routing_control_state.rs`, and inline in `get_routing_control_state.rs` and `update_routing_control_states.rs`;
3. the `RoutingControlState` enum refusal — written in both update handlers.

That the copies drift is not hypothetical: copy (3) has ALREADY drifted. The single-update handler words the `ValidationExceptionField.message` "Member must satisfy enum value set: [...]" while the batch handler words the same field "RoutingControlState must satisfy enum value set: [...]" — one logical refusal, two wordings, in sibling handlers of one crate.

The fix is the idiom the crate already uses: constructors returning the SHARED `ops::types` structs (`ValidationException`, `ResourceNotFoundException`) for these three refusals, with each handler keeping its own `…ErrorModeled` wrap. Every response and every existing test stays byte-identical for the paths tests reach (the enum-refusal branches are unreachable behind the boundary validator, which is where the crate's own tests observe that refusal).

## Evidence

Copy A of the safety-rule mapping — services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/src/ops/update_routing_control_state.rs:270-282:
```
        if let Some(violation) = plane.safety_rule_violation(
            &proposed,
            input.safety_rules_to_override.as_deref(),
        ) {
            return Err(
                UpdateRoutingControlStateErrorModeled::ValidationException(ValidationException {
                    message: violation.message,
                    reason: Some("other".to_string()),
                    fields: None,
                })
                .into(),
            );
        }
```
Copy B, the same rule again — src/ops/update_routing_control_states.rs:341-352:
```
        if let Some(violation) =
            plane.safety_rule_violation(&resolved, input.safety_rules_to_override.as_deref())
        {
            return Err(
                UpdateRoutingControlStatesErrorModeled::ValidationException(ValidationException {
                    message: violation.message,
                    reason: Some("other".to_string()),
                    fields: None,
                })
                .into(),
            );
        }
```
The idiom the crate already uses for exactly this, which these two paths skip — src/service/resource_arn.rs:123-130:
```
pub fn invalid_resource_arn() -> ValidationException {
    ValidationException {
        message: super::refusal::INVALID_RESOURCE_ARN_MESSAGE.to_string(),
        reason: None,
        fields: None,
    }
}
```

The drift this duplication has already produced — src/ops/update_routing_control_state.rs:233-239:
```
                    fields: Some(vec![ValidationExceptionField {
                        name: "RoutingControlState".to_string(),
                        message: format!(
                            "Member must satisfy enum value set: [{}]",
                            ROUTING_CONTROL_STATE_VALUES.join(", ")
                        ),
                    }]),
```
versus the sibling, src/ops/update_routing_control_states.rs:278-285 + 407-419, where `validation_exception()` copies the top-level message into the field:
```
                return Err(validation_exception(
                    format!(
                        "RoutingControlState must satisfy enum value set: [{}]",
                        ROUTING_CONTROL_STATE_VALUES.join(", ")
                    ),
                    "fieldValidationFailed",
                    "UpdateRoutingControlStateEntries.RoutingControlState",
                ));
```
```
        fields: Some(vec![ValidationExceptionField {
            name: field.to_string(),
            message,
        }]),
```
So the same logical refusal names its field "Member must satisfy enum value set: [Off, On]" in one handler and "RoutingControlState must satisfy enum value set: [Off, On]" in the other.

The not-found triple: src/ops/get_routing_control_state.rs:215-224 (inline), src/ops/update_routing_control_state.rs:306-312 (`fn not_found`), src/ops/update_routing_control_states.rs:303-315 (inline) — all three repeating `message: crate::state::routing_control_not_found_message(..)`, `resource_id: <the arn>`, `resource_type: crate::state::ROUTING_CONTROL_RESOURCE_TYPE.to_string()`.

Tests that already cover these responses and must stay unchanged: tests/all/safety_rule_refusal_message.rs:116 ("an update a deployed safety rule refuses is answered with HTTP 400", asserting the whole message) and :329 ("the batch operation words the same refusal the same way"); tests/all/routing_control_toggle.rs:162 ("an ARN no control-plane routing control was minted for is ResourceNotFoundException") and :116 ("a state value outside the modelled RoutingControlState value set is refused with HTTP 400 ValidationException", which asserts status and code only).

## Steps to Reproduce

1. Open src/ops/update_routing_control_state.rs:270-282 and src/ops/update_routing_control_states.rs:341-352 side by side: the safety-rule refusal (modelled class, reason "other", fields None) is written out twice.
2. Compare src/service/resource_arn.rs:123-130, which holds the ARN refusal's shape in ONE shared constructor that each handler only wraps — the pattern the two blocks above skip.
3. Compare the field wording of the enum refusal in src/ops/update_routing_control_state.rs:233-239 ("Member must satisfy enum value set: …") with src/ops/update_routing_control_states.rs:278-285 plus the helper at 407-419 ("RoutingControlState must satisfy enum value set: …"): one rule, two wordings — the drift the duplication allows.
4. Compare the three copies of the routing-control not-found construction: src/ops/get_routing_control_state.rs:215-224, src/ops/update_routing_control_state.rs:306-312, src/ops/update_routing_control_states.rs:303-315.
5. Run the suites that pin these responses (tests/all/safety_rule_refusal_message.rs, tests/all/routing_control_toggle.rs) before and after holding each refusal in one shared constructor: the responses and the assertions are unchanged.
