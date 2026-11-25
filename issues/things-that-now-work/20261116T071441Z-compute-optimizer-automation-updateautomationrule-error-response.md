Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: e61f7393df24006a0e171f97939683922fa25853

# UpdateAutomationRule judges `priority` before `name` while CreateAutomationRule judges `name` before `priority`, so one request breaking both is refused two different ways

_Filed: 2026-11-16T07:14:41Z_
_Operation: UpdateAutomationRule | Class: error-response_

> _Note: this issue was identified against the code base at commit `e61f7393df24006a0e171f97939683922fa25853`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateAutomationRule` and `UpdateAutomationRule` are the two write paths for a rule's `name` and `priority`. Each individual rule is single-homed in `crate::state` (`rule_name_violation`, `priority_violation`), but the ORDER the two handlers apply them in is swapped: the create reports the `name` floor first and the `priority` form later, the update reports the `priority` form first and the `name` floor later. A request that breaks both at once (e.g. `name: ""` together with `priority: "7"`) is therefore refused with the `name` report by the create and with the `priority` report by the update — the same bad request, two different `InvalidParameterValueException` bodies.

This is exactly the divergence this crate's own state module says single-homing exists to prevent, and it is the only reason-free swap between the two clause lists: the remaining shared clauses (`recommendedActionTypes`, `schedule`, `criteria`) are applied in the same relative order on both paths, and the `organizationConfiguration` clause sits last on the update only because the merged value needs the stored rule. Nothing in either handler, in `crate::state`, or in the tests gives a reason for the name/priority swap — both update clauses are even commented as being "judged where the create judges it".

The test that owns this invariant drives each row through both write paths and asserts the two messages are equal, but every row mutates exactly ONE member, so the two-member case that exposes the swap is untested.

## Evidence

CreateAutomationRule checks `name` first, `priority` third — services/.../src/ops/create_automation_rule.rs:450-471:
```
fn invalid_parameter(input: &CreateAutomationRuleInput) -> Option<String> {
    if let Some(report) = crate::state::rule_name_violation(&input.name) {
        return Some(report);
    }
    if let Some(report) = crate::state::organization_configuration_violation(
        &input.rule_type,
        input.organization_configuration.as_ref(),
    ) {
        return Some(report);
    }
    if let Some(report) = input
        .priority
        .as_deref()
        .and_then(crate::state::priority_violation)
    {
        return Some(report);
    }
```

UpdateAutomationRule checks `priority` first and `name` second — services/.../src/ops/update_automation_rule.rs:224-243:
```
        // `priority` is a plain `String` in the model but documents a decimal
        // between 0 and 1, so its form is judged where the create judges it
        // (`crate::state::priority_violation`).
        if let Some(report) = input
            .priority
            .as_deref()
            .and_then(crate::state::priority_violation)
        {
            return Err(invalid_parameter(report));
        }
        // "Must be 1-128 characters long", of which the floor is the service's
        // own and is judged where the create judges it
        // (`crate::state::rule_name_violation`).
        if let Some(report) = input
            .name
            .as_deref()
            .and_then(crate::state::rule_name_violation)
        {
            return Err(invalid_parameter(report));
        }
```

The crate states the rule these two break — services/.../src/state.rs:3022-3031 (`tag_list_violation`):
```
/// A list is judged by both rules this service owns over a `tags` member, in ONE
/// order: ... Ordering the rules HERE instead
/// of in each handler is what makes a list that breaks more than one of them
/// refused with one and the same report whichever operation received it: two
/// handlers applying the same two rules in opposite orders word the same refusal
/// differently, which is exactly what single-homing the wording in this module is
/// for.
```

And both helpers claim the two paths cannot drift over how they word the refusal — services/.../src/state.rs:3086-3088:
```
/// Both operations that write a rule's name — the create that requires it and
/// the update that REPLACES it — judge it here, so the two cannot drift apart
/// over which names they refuse or over how they word the refusal.
```
services/.../src/state.rs:4084-4088:
```
/// The form is the documented one ([`priority_is_valid`]). Both operations that
/// write a rule's precedence — the create and the update that replaces it —
/// judge it here, so the two cannot drift apart over which values they refuse,
/// over the bound and the fractional width they refuse them at, or over how they
/// word the refusal.
```

Test coverage, and why it misses this: services/.../tests/all/rule_name_and_priority_rules.rs:6-10 states the invariant —
```
//! The rule under test: a value a request stores into a rule's `name` or its
//! `priority` is judged by the same requirement whichever operation carries it —
//! `CreateAutomationRule`, which requires the name and may carry a priority, and
//! `UpdateAutomationRule`, which REPLACES either — so the two accept the same
//! values, refuse the same values, and word the refusal with the same report:
```
but every row sets exactly one member, services/.../tests/all/rule_name_and_priority_rules.rs:110-119:
```
    let mut create_request = minimal_rule(&format!("subject-{case}"));
    create_request[member_name] = value.clone();
    ...
    let mut update_request = json!({"ruleArn": &subject.arn, "ruleRevision": subject.revision});
    update_request[member_name] = value.clone();
```
so no row carries a bad `name` AND a bad `priority` together, and the swap is unpinned.

## Steps to Reproduce

1. POST / with `X-Amz-Target: ComputeOptimizerAutomationService.UpdateEnrollmentConfiguration` and `{"status":"Active"}` to open the enrollment gate.
2. POST `CreateAutomationRule` with a valid body but BOTH a bad name and a bad priority: `{"name":"","ruleType":"AccountRule","recommendedActionTypes":["UpgradeEbsVolumeType"],"schedule":{"scheduleExpression":"30 12 * * ? *"},"status":"Active","priority":"7"}`. Observe 400 InvalidParameterValueException whose message is the `name` report ("Value at 'name' failed to satisfy constraint: Member must have length greater than or equal to 1").
3. POST `CreateAutomationRule` with a valid name to mint a rule, and note its `ruleArn`/`ruleRevision`.
4. POST `UpdateAutomationRule` with `{"ruleArn":<arn>,"ruleRevision":<rev>,"name":"","priority":"7"}` — the same two broken values.
5. Observe 400 InvalidParameterValueException whose message is the `priority` report ("Value at 'priority' failed to satisfy constraint: Member must be a decimal number between 0 and 1 with up to 30 digits after the decimal point"), not the `name` report the create answered with — the two write paths word the same refusal differently.
