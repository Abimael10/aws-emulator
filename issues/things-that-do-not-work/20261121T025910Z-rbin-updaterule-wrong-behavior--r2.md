Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 0c5cb36cb8e8253fc1120b6ad7fe39ce6f6094b8

# An UpdateRule carrying no optional member still returns an `available` rule to `pending`, although the handler's own rule is that an absent member leaves the stored value alone

_Filed: 2026-11-21T02:59:10Z_
_Operation: UpdateRule | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `0c5cb36cb8e8253fc1120b6ad7fe39ce6f6094b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateRuleRequest` declares exactly one required member (`Identifier`, an `@httpLabel`); every other member is optional, so `PATCH /rules/{Identifier}` with an empty body is a request the model accepts and the crate's own test suite exercises. The handler's merge comment states the rule it means to follow — only the members the request carried are applied, an absent member leaves the stored value alone — and with every optional member absent the merge does exactly that: `merged_resource_tags` / `merged_exclude_resource_tags` fall back to the stored lists, and the `retention_period` / `description` writes are skipped.

But the handler then calls `rule.begin_pending(now)` unconditionally, which writes two stored values the request named nothing about: `status` becomes `pending` and `available_at` is pushed one `STATUS_TRANSITION` into the future. The comment above that call justifies it by the rule having "just been modified" — on this path nothing was modified. Because the crate (and the model) hold that "Only retention rules that are in the `available` state retain resources", a settled, retaining rule is taken out of the retaining state by a request that changed none of its configuration, and a `GetRule` straight afterwards reports `pending` for a rule nobody changed.

The same applies to a request that re-sends the values the rule already holds (e.g. `ResourceTags` identical to the stored list): the merge is a no-op and the status still flips. The fix is to make `begin_pending` conditional on the merge having actually changed the rule, as the comment above it already states.

## Evidence

The rule the handler states for itself — `services/service-com-amazonaws-rbin-amazonrecyclebin/src/ops/update_rule.rs:211-214`:
```
        // Only the members the request carried are applied; an absent member leaves the
        // stored value alone. `ResourceType` is deliberately NOT applied: the model says
        // "This parameter is currently not supported. You can't update a retention
        // rule's resource type after creation."
```

The code under it that breaks the rule for a request whose optional members are ALL absent — `src/ops/update_rule.rs:314-325`:
```
        if let Some(retention_period) = input.retention_period {
            rule.retention_period = retention_period;
        }
        if let Some(description) = input.description {
            rule.description = Some(description);
        }
        rule.resource_tags = merged_resource_tags;
        rule.exclude_resource_tags = merged_exclude_resource_tags;
        // A rule that has just been modified is not retaining resources under the new
        // configuration yet: it goes back to `pending` and reaches `available` once the
        // clock makes the transition due.
        rule.begin_pending(now);
```
where `merged_resource_tags` / `merged_exclude_resource_tags` are the STORED lists on this path — `src/ops/update_rule.rs:242-251`:
```
                input
                    .resource_tags
                    .unwrap_or_else(|| rule.resource_tags.clone()),
                input
                    .exclude_resource_tags
                    .unwrap_or_else(|| rule.exclude_resource_tags.clone()),
```

What `begin_pending` writes — `src/state.rs:464-467`:
```
    pub fn begin_pending(&mut self, now: SystemTime) {
        self.status = RULE_STATUS_PENDING.to_string();
        self.available_at = status_transition_end(now);
    }
```
and why that matters — `src/state.rs:278-280`:
```
/// The two states `RuleStatus` models, kept here so every status-aware operation
/// names one spelling. "The state of the retention rule. Only retention rules that
/// are in the `available` state retain resources."
```
(the same sentence the model carries: smithy_query(query="operation", operation="UpdateRule") output member `Status` -> `RuleStatus`, "The state of the retention rule. Only retention rules that are in the available state retain resources.")

That an empty patch is an accepted request is pinned by the crate's own test — `tests/all/retention_rule_lifecycle.rs:683`:
```
        ("nothing at all", serde_json::json!({})),
```
but that test asserts only `RetentionPeriod`, `Description`, `ResourceTags` and `ResourceType` (lines 718-738) and never `Status`, and the status test deliberately covers only patches that DO change something — `tests/all/rule_status_transitions.rs:225-226`:
```
    // Each row modifies a different part of the rule; the rule under test holds for
    // every completed modification, whatever it changed.
```
so no existing test pins the no-op case either way.

## Steps to Reproduce

1. Start the emulator with a fresh rbin store.
2. POST /rules with {"ResourceType":"EBS_SNAPSHOT","RetentionPeriod":{"RetentionPeriodValue":7,"RetentionPeriodUnit":"DAYS"}} — HTTP 201, Status="pending"; note the Identifier.
3. Wait longer than crate::state::STATUS_TRANSITION (300 ms) and GET /rules/{Identifier} — HTTP 200, Status="available" (the rule is retaining).
4. PATCH /rules/{Identifier} with the empty body {} — a request that names no optional member, so per src/ops/update_rule.rs:211-214 every stored value must be left alone.
5. Expected: HTTP 200 reporting the rule unchanged, Status="available".
6. Actual: HTTP 200 with Status="pending" — begin_pending (src/ops/update_rule.rs:325) rewrote status and available_at although nothing was modified.
7. GET /rules/{Identifier} immediately — Status="pending", so the flip is persisted and observable to a later caller; by src/state.rs:278-280 the rule has stopped retaining resources for the transition span.
8. The same happens for a patch that re-sends the stored values, e.g. {"ResourceTags":[<the exact list GetRule reports>]}.
