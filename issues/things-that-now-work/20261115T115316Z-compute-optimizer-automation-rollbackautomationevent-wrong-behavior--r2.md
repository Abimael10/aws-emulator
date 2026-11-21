Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023

# RollbackAutomationEvent skips the event retrieval window its sibling eventId readers enforce

_Filed: 2026-11-15T11:53:16Z_
_Operation: RollbackAutomationEvent | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other operation that reaches an automation event by `eventId` resolves it through `EventStore::retrievable_mut`, which refuses an event created before the retrieval window (`EVENT_RETRIEVAL_WINDOW` before now) exactly as it refuses an id the store never held. `RollbackAutomationEvent` resolves the same `eventId` with the raw `EventStore::get` / `get_mut`, so it applies no window check at all.

Consequence: for an event whose `createdTimestamp` is older than the retrieval window, `GetAutomationEvent` and `ListAutomationEventSteps` answer `ResourceNotFoundException` (404) and `ListAutomationEvents`/`ListAutomationEventSummaries` neither report nor count it, while `RollbackAutomationEvent` answers 200, flips the stored `eventStatus` to `RollbackInProgress`, appends the rollback steps and arms a pending transition. The caller is told a rollback started on an event the very same service reports as non-existent, and the state the rollback wrote is observable through no reader — the store is mutated behind a 404. The crate's own rule, stated on the lookup helper the siblings use, is that such an event is "outside every query's reach"; this handler is the one eventId-scoped path that ignores it.

## Evidence

The rule, on the lookup every reporting eventId reader uses — `src/state.rs:2457`-`2467`:
```
    /// The event an `eventId` names, as a reader that REPORTS it reaches one:
    /// only while the event is still retrievable.
    ///
    /// An event created before the retrieval window ([`EVENT_RETRIEVAL_WINDOW`]
    /// before `now`) is outside every query's reach, so a per-id reader answers
    /// for it exactly as it answers for an id this store never held — nothing
    /// a listing has dropped is reportable through an id.
```
…implemented at `src/state.rs:2468`-`2480`:
```
    pub fn retrievable_mut(&mut self, event_id: &str, now: SystemTime) -> Option<&mut StoredEvent> {
        let window_start = effective_window_start(None, now);
        let event = self.by_id.get_mut(event_id)?;
        if !within_window(event.created_timestamp, Some(window_start), None) {
            return None;
        }
```

Sibling handlers go through it:
- `src/ops/get_automation_event.rs:214`: `let Some(event) = guard.events.retrievable_mut(&input.event_id, now) else {` … returns `ResourceNotFoundException`.
- `src/ops/list_automation_event_steps.rs:226`: `let Some(event) = guard.events.retrievable_mut(&input.event_id, now) else { return Err(not_found()); };`

This handler does not — `src/ops/rollback_automation_event.rs:262`-`268` and `:281`-`283`:
```
        let Some(account_id) = guard
            .events
            .get(&input.event_id)
            .map(|event| event.account_id.clone())
        else {
            return Err(not_found());
        };
...
        let Some(event) = guard.events.get_mut(&input.event_id) else {
            return Err(not_found());
        };
```
and then writes it at `:298`: `event.begin_rollback(now);`

Test coverage: `tests/all/automation_event_retrieval_window.rs:6`-`12` states the rule and covers only the readers — "an automation event is reportable only while it was created inside the retrieval window … so an event created before that floor is out of reach of EVERY query of the store — the per-`eventId` readers refuse it with the modelled `ResourceNotFoundException` (404), as they do an id the store never held, and the listings neither report nor count it however far back the caller's start bound points." Its table only drives `GetAutomationEvent`, `ListAutomationEventSteps`, `ListAutomationEvents` and `ListAutomationEventSummaries` (lines 227-278). `tests/all/automation_event_rollback.rs:85`-`107` seeds every rollback case with `created_timestamp: DateTime::from(std::time::SystemTime::now())`, so no test pins rollback behaviour for an event outside the window.

## Steps to Reproduce

1. Seed one event into the shared store exactly as `tests/all/automation_event_retrieval_window.rs` does (`state::service_state(&engine).lock().events.insert(..)`), with `event_status` = `state::EVENT_STATUS_COMPLETE`, `pending: None`, and `created_timestamp` set to `now - EVENT_RETRIEVAL_WINDOW - 60s`.
2. Enroll the caller (`UpdateEnrollmentConfiguration` with status `Active`).
3. Call `GetAutomationEvent` with that `eventId` — observe HTTP 404 `ResourceNotFoundException` (the window floor), and `ListAutomationEvents` with no bounds does not list it.
4. Call `RollbackAutomationEvent` with the same `eventId` — observe HTTP 200 with `eventStatus` `RollbackInProgress`, i.e. a rollback accepted for an event the service reports as not found.
5. Call `GetAutomationEvent` and `ListAutomationEventSteps` again — still 404, so the appended rollback steps and the changed status are unobservable state the rollback wrote behind a not-found.
