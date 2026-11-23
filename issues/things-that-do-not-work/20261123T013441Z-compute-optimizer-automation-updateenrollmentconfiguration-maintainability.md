Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 6c5e437ac482b043e6839b0d94a89be6f95b8112

# Single-home UpdateEnrollmentConfiguration's response projection, which the handler writes out twice

_Filed: 2026-11-23T01:34:41Z_
_Operation: UpdateEnrollmentConfiguration | Class: maintainability_

> _Note: this issue was identified against the code base at commit `6c5e437ac482b043e6839b0d94a89be6f95b8112`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other operation of this crate that answers both a fresh call and an idempotent replay builds its response in exactly ONE place and says so in the helper's doc comment (`output_for`). `UpdateEnrollmentConfiguration` is the exception: the projection from the stored `EnrollmentConfig` to `UpdateEnrollmentConfigurationOutput` — the same three members plus the fallback for the REQUIRED `lastUpdatedTimestamp` that a stored entry may not carry — is written out twice in the one handler, once in the `TokenOutcome::Replay` arm and once at the end of the fresh path. The two copies have ALREADY drifted in the one expression they differ in: the replay copy falls back with `unwrap_or_else(crate::state::now)`, the fresh copy with `unwrap_or(stored_at)`. That is precisely the drift the sibling handlers' "ONE place" rule exists to prevent: a later change to what the response reports (a member added to the output shape, or a change to how the missing-instant fallback is chosen) has to be made in two places, and nothing in the handler forces the second one. Collapsing both into one `fn output_for(stored: crate::state::EnrollmentConfig, at: ::aws_smithy_types::DateTime) -> UpdateEnrollmentConfigurationOutput`, with the replay arm passing `crate::state::now()` and the fresh path passing `stored_at`, leaves every response byte-for-byte unchanged and every existing test passing.

## Evidence

Copy 1 — the replay arm, services/service-com-amazonaws-computeoptimizerautomation-computeoptimizerautomationservice/src/ops/update_enrollment_configuration.rs:223-232:
```
            crate::state::TokenOutcome::Replay(_) => {
                let stored = guard.enrollment(&account_id);
                return Ok(UpdateEnrollmentConfigurationOutput {
                    status: stored.status,
                    status_reason: stored.status_reason,
                    last_updated_timestamp: stored
                        .last_updated_timestamp
                        .unwrap_or_else(crate::state::now),
                });
            }
```
Copy 2 — the fresh path, same file:298-304:
```
        // Read back what was just stored, rather than echoing the request.
        let stored = guard.enrollment(&account_id);
        Ok(UpdateEnrollmentConfigurationOutput {
            status: stored.status,
            status_reason: stored.status_reason,
            last_updated_timestamp: stored.last_updated_timestamp.unwrap_or(stored_at),
        })
```
The rule the siblings state, src/ops/create_automation_rule.rs:386-389:
```
/// The response for a stored rule. The ONE place this operation's output is
/// built, so every path (fresh create and idempotent replay) answers from the
/// same stored fields.
fn output_for(rule: &crate::state::StoredRule) -> CreateAutomationRuleOutput {
```
and src/ops/start_automation_event.rs:309-311:
```
/// The response for a stored event. The ONE place this operation's output is
/// built, so a fresh start and an idempotent replay answer from the same stored
/// fields.
```
(the same comment appears at src/ops/rollback_automation_event.rs:328-330 and src/ops/update_automation_rule.rs:450-452).

Test coverage that pins both copies: tests/all/enrollment_write_timestamps.rs:277-328 (`a_repeat_of_the_stored_status_persists_nothing_whatever_token_it_carries`) walks the rows `("no-token", None)`, `("a-fresh-token", ...)` and `("the-first-calls-token", Some("enroll-repeat-first"))` — the last row drives the Replay arm and asserts `same_instant(repeated, stamped)` against the fresh path's own answer; tests/all/error_payloads.rs:675-752 drives the same token with a different status (parameter mismatch).

## Steps to Reproduce

1. Open services/service-com-amazonaws-computeoptimizerautomation-computeoptimizerautomationservice/src/ops/update_enrollment_configuration.rs and read lines 219-305: the `UpdateEnrollmentConfigurationOutput { status, status_reason, last_updated_timestamp }` projection appears at 225-231 and again at 300-304, with different fallbacks for the required timestamp.
2. Compare with src/ops/create_automation_rule.rs:386-406, src/ops/update_automation_rule.rs:450-469, src/ops/start_automation_event.rs:309-318 and src/ops/rollback_automation_event.rs:328-336, where the fresh and replay paths share one `output_for` helper whose doc comment states it is 'The ONE place this operation's output is built'.
3. Confirm the behaviour is pinned by tests/all/enrollment_write_timestamps.rs:277-328 (the 'the-first-calls-token' row exercises the Replay copy) and tests/all/error_payloads.rs:675-752.
4. Extract one `output_for(stored, at)` helper used by both arms (replay passing `crate::state::now()`, fresh passing `stored_at`) and re-run the suite: every response and every test stays unchanged.
