Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: cc91cd412557a223e827e0d947b209de54e44d58

# An update that writes no member of the rule still stamps lastUpdatedTimestamp and bumps ruleRevision, unlike every other write path in the crate

_Filed: 2026-11-25T02:52:08Z_
_Operation: UpdateAutomationRule | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `cc91cd412557a223e827e0d947b209de54e44d58`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateAutomationRule` applies only the members the request carries (`ruleArn` and `ruleRevision` are the only required ones), and then unconditionally bumps `ruleRevision` and overwrites `lastUpdatedTimestamp` with the store instant — even when the request carries no mutable member at all, or carries values identical to the ones already stored. So `UpdateAutomationRule{ruleArn, ruleRevision}` is served 200, every later reader (`GetAutomationRule`, `ListAutomationRules`, the update's own response) reports a NEW `lastUpdatedTimestamp` at which nothing about the rule was updated, and the caller's optimistic-concurrency token is invalidated for a call that changed nothing.

This is the one writer of `StoredRule::last_updated_timestamp` that does not first ask whether the stored value actually changed. The crate single-homes the opposite rule and states it as shared by every writer of that field: `StoredRule::set_tags` is the helper the two tagging operations write a rule through precisely so a no-op write moves no timestamp, and `set_enrollment_status`/`associate_account`/`disassociate_account` follow the same rule, as does the opt-out loop in `UpdateEnrollmentConfiguration`, which stamps only the rules whose status it changes. The crate's own tests pin the "changes nothing → updated nothing" half for the tag payloads and for the enrollment writes; `rule_change_stamps.rs` states that rule generally but asserts only the changing half for the update, which is how the update path slipped past it.

## Evidence

Both sides are in this crate.

The unconditional write — `src/ops/update_automation_rule.rs:375-404`:
```
        // Only the members the request carries are applied; an absent member
        // leaves the stored value alone.
        if let Some(name) = input.name.clone() { rule.name = name; }
        ...
        // "The revision number ... is incremented each time the rule is updated."
        rule.rule_revision += 1;
        rule.last_updated_timestamp = updated_at;
```
Nothing between the revision check and these two lines asks whether any member actually changed, so a request carrying only `ruleArn` + `ruleRevision` reaches them.

The rule it breaks — `src/state.rs:1185-1207` (`StoredRule::set_tags`, the helper the tagging paths write a rule through):
```
    /// Replace this rule's tag list with `tags`, stamping
    /// [`Self::last_updated_timestamp`] with `at` when that write actually
    /// changes the rule.
    ...
    /// A request that leaves the list exactly as it stood updated nothing —
    /// re-adding the tags the rule already carries, removing a key it does not
    /// carry — so it moves no timestamp, the same way an opt-out stamps only the
    /// rules whose status it changes.
```
and `src/state.rs:3783-3790` (`set_enrollment_status`), which names that rule as shared by every writer of the field:
```
    /// Answers whether the stored configuration CHANGED. `at` is stamped as the
    /// account's `lastUpdatedTimestamp` ... only when it did — the same rule the
    /// other writers of this field follow ([`Self::associate_account`],
    /// [`Self::disassociate_account`], [`StoredRule::set_tags`]) — so a repeat of
    /// the status the account already reports leaves its stored configuration,
    /// timestamp included, exactly where it was, and no reader claims an instant
    /// at which nothing was updated.
```

Test coverage of the sibling halves: `tests/all/tag_write_timestamps.rs:281-285` — `"case {name:?}: a write that changes nothing updated nothing, expected lastUpdatedTimestamp to stay {before_write}, got {stamped}"`; `tests/all/enrollment_write_timestamps.rs:225` carries the same assertion for the enrollment write. `tests/all/rule_change_stamps.rs:69-73` states the rule and explicitly leaves the no-op half of it to those two suites: "Every row's write CHANGES the stored rule, so every row must leave a later `lastUpdatedTimestamp` behind; the other half of the rule — a call that leaves the rule exactly as it stood moves nothing — is asserted by [`an_opt_out_stamps_only_the_rules_it_stops`] below and, for the tag payloads, by [`super::tag_write_timestamps`]." No test exercises an `UpdateAutomationRule` that writes no member, so nothing pins the current behaviour.

## Steps to Reproduce

1. Start the emulator and call UpdateEnrollmentConfiguration with status=Active (awsJson1_0, POST / with X-Amz-Target: ComputeOptimizerAutomationService.<Op>).
2. Call CreateAutomationRule with a minimal valid rule; note the returned ruleArn, ruleRevision (1) and createdTimestamp.
3. Call GetAutomationRule for that ruleArn and note lastUpdatedTimestamp (= createdTimestamp).
4. Wait long enough for the clock to move, then call UpdateAutomationRule with ONLY {ruleArn, ruleRevision: 1} — no other member.
5. Observe HTTP 200 with ruleRevision 2 and a lastUpdatedTimestamp later than the one read in step 3, although no member of the rule was written.
6. Contrast with the sibling paths: TagResource re-adding exactly the tags the rule already carries leaves lastUpdatedTimestamp where it was (StoredRule::set_tags), and UpdateEnrollmentConfiguration repeating the status the account already reports leaves its lastUpdatedTimestamp where it was (set_enrollment_status).
