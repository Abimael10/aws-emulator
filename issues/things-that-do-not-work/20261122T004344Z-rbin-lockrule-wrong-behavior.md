Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 97025d55c5c1c7554c95fdf98ddaa852bf21abc7

# LockRule replaces the unlock delay of a rule inside its unlock delay, the very change it refuses on a `locked` rule

_Filed: 2026-11-22T00:43:44Z_
_Operation: LockRule | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `97025d55c5c1c7554c95fdf98ddaa852bf21abc7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`LockRule` refuses a request that would CHANGE the lock in force on a rule whose stored `lock_state` is exactly `locked` (409 `ConflictException`, condition "has a different unlock delay already set"), citing the crate's own rule that "a locked rule can't be modified". But the guard tests the stored token alone (`rule.lock_state.as_deref() == Some(LOCK_STATE_LOCKED)`), not the crate's own predicate for "this rule's lock still forbids modifying it" (`Rule::is_lock_protected`).

A rule in `pending_unlock` that is still inside its unlock delay therefore falls through that guard, and the handler overwrites `rule.lock_configuration` with whatever `UnlockDelay` the request names. By the crate's own definition that rule "is still locked" to the caller — it is exactly the state for which `UpdateRule` and `DeleteRule` answer 409 with the condition "is locked" at that same instant.

The effect is that the unlock delay a lock guarantees can be shortened without waiting it out: lock a rule with a 30-day `UnlockDelay`, call `UnlockRule` (rule is now `pending_unlock`, 30 days of protection left), then immediately call `LockRule` with a 7-day `UnlockDelay`. The handler serves it (HTTP 200), the rule is `locked` under 7 days, and a second `UnlockRule` makes the rule modifiable 7 days from now instead of 30 — while the direct path (`LockRule` with 7 days on the still-`locked` rule) is refused as a change to the lock in force.

Note the narrower sub-case only: re-locking a `pending_unlock` rule with the SAME delay is the documented "locking during the unlock delay period returns the rule to `locked`" path (src/ops/lock_rule.rs:240-243) and must keep working; what is inconsistent is serving a DIFFERENT delay there.

## Evidence

The crate's own predicate for "the lock still forbids modifying this rule", which answers TRUE for `pending_unlock` inside the delay — `src/state.rs:369-383`:
```
    /// Whether the rule's lock still forbids modifying or deleting it.
    ///
    /// Per the model: `locked` cannot be modified or deleted; `pending_unlock` has
    /// been unlocked but is still inside the unlock delay, so it can be modified or
    /// deleted "only after the unlock delay period has expired" — which the clock,
    /// not a caller, makes due.
    pub fn is_lock_protected(&self, now: SystemTime) -> bool {
        match self.lock_state.as_deref() {
            Some(LOCK_STATE_LOCKED) => true,
            ...
            Some(LOCK_STATE_PENDING_UNLOCK) => self.lock_end_time.is_some_and(|end| now < end),
```

and the crate stating that such a rule IS locked as far as a caller is concerned — `src/state.rs:749-757` (`BlockingCondition::Locked`):
```
    /// The rule's lock still forbids modifying or deleting it.
    ///
    /// Reported for `locked` AND for `pending_unlock` while the unlock delay is still
    /// running — the model says such a rule "can be modified or deleted only after the
    /// unlock delay period has expired", so to the caller it is still locked.
```

The guard in `LockRule` that cites that rule but keys on the stored token instead — `src/ops/lock_rule.rs:226-238`:
```
        // What is left is a request that would CHANGE the lock in force on a rule that is
        // already locked — a different unlock delay — and a locked rule "can't be
        // modified", so the lock request can't be completed.
        if rule.lock_state.as_deref() == Some(crate::state::LOCK_STATE_LOCKED) {
            return Err(LockRuleErrorModeled::ConflictException(
                crate::state::rule_state_conflict(
                    crate::state::BlockedAction::Lock,
                    &rule.identifier,
                    crate::state::BlockingCondition::DifferentUnlockDelay,
                ),
            )
            .into());
        }
```
and the write a `pending_unlock` rule reaches instead — `src/ops/lock_rule.rs:244-246`:
```
        rule.lock_configuration = Some(input.lock_configuration);
        rule.lock_state = Some(crate::state::LOCK_STATE_LOCKED.to_string());
        rule.lock_end_time = None;
```

The sibling handlers that DO use the crate's predicate on the same rule at the same instant — `src/ops/update_rule.rs:229-241`:
```
            // A locked rule "can't be modified or deleted", and an unlocked one still
            // inside its unlock delay can be modified "only after the unlock delay period
            // has expired" — to the caller, both are a rule that is locked.
            if rule.is_lock_protected(now) {
                return Err(UpdateRuleErrorModeled::ConflictException(
```
and `src/ops/delete_rule.rs:158-170`:
```
        // A locked rule "can't be modified or deleted", and an unlocked one still
        // inside its unlock delay can be deleted "only after the unlock delay period
        // has expired" — to the caller, both are a rule that is locked.
        if rule.is_lock_protected(now) {
            return Err(DeleteRuleErrorModeled::ConflictException(
```

Test coverage, and why the case slips through: `tests/all/error_response_bodies.rs:709-780` tabulates every 409 this service answers. It has a LockRule row for the `locked` rule (lines 724-730, `"LockRule on a locked rule, under another unlock delay"` / `predicate: "has a different unlock delay already set"` / `lock_state: Some("locked")`) and `UpdateRule`/`DeleteRule` rows for the rule inside its unlock delay (lines 745-758, `predicate: "is locked"` / `lock_state: Some("pending_unlock")`) — but NO `Action::Lock` row for that `pending_unlock` rule. `tests/all/rule_lock_transitions.rs:3-14` states its own scope as "a lock request against a locked rule is refused only when it names ANOTHER unlock delay", and its full-cycle case (line 579, `expected_states = ["locked", "pending_unlock", "locked"]`, lines 579-599) re-locks with the same configuration, never a different delay.

## Steps to Reproduce

1. Start the emulator with a fresh rbin store.
2. POST /rules with {"ResourceType":"EBS_SNAPSHOT","RetentionPeriod":{"RetentionPeriodValue":7,"RetentionPeriodUnit":"DAYS"},"LockConfiguration":{"UnlockDelay":{"UnlockDelayValue":30,"UnlockDelayUnit":"DAYS"}}} — HTTP 201, LockState=locked, UnlockDelayValue=30.
3. PATCH /rules/{Identifier}/unlock — HTTP 200, LockState=pending_unlock, LockEndTime ~30 days out.
4. Confirm the rule is lock-protected right now: PATCH /rules/{Identifier} with {"Description":"x"} — HTTP 409 ConflictException, Message "Cannot modify Recycle Bin rule {Identifier} because it is locked."; DELETE /rules/{Identifier} — HTTP 409, "... because it is locked."
5. PATCH /rules/{Identifier}/lock with {"LockConfiguration":{"UnlockDelay":{"UnlockDelayValue":7,"UnlockDelayUnit":"DAYS"}}}.
6. Actual: HTTP 200 — LockState=locked and LockConfiguration.UnlockDelay.UnlockDelayValue=7. The lock in force on a rule the crate's own `is_lock_protected`/`BlockingCondition::Locked` call "still locked" was changed.
7. Contrast: run step 5 against a rule left in `locked` (skip step 3) — HTTP 409 ConflictException, "Cannot lock Recycle Bin rule {Identifier} because it has a different unlock delay already set."
8. GET /rules/{Identifier} then PATCH /rules/{Identifier}/unlock again — LockEndTime is now ~7 days out, so the 30-day unlock delay the lock was taken under has been reduced to 7 without ever waiting it out.
