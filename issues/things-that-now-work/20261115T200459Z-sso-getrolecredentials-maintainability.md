Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 51772985bc72cfea1a888e0d513791715cd162c8

# Single-home the pre-handler expiry sweep: the two service seams each carry their own copy of it

_Filed: 2026-11-15T20:04:59Z_
_Operation: GetRoleCredentials | Class: maintainability_

> _Note: this issue was identified against the code base at commit `51772985bc72cfea1a888e0d513791715cd162c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The rule "before this service answers a request, end every minted role credential the clock has already made spent" is single-homed as `crate::state::end_expired_credentials`, exactly as `state.rs` says the shared rules are. But the CALL SITE for the two pre-handler seams is not: `service/session_gate.rs` and `service/unreadable_request.rs` each carry a byte-identical copy of the same three-statement block (reach the store through `service_state(&*self.engine)`, take the lock, call `end_expired_credentials(&mut state, ctx.credential_store(), SystemTime::now())`) under a near-identical six-line comment. Two copies of one rule can drift, and in this crate they already have: `EmulatedService::override_routing_error` (`src/service.rs:457-515`) is the THIRD seam that answers a portal request without reaching a handler, and it carries no copy at all — so the "one write every portal request makes" that both copies claim is not made for the requests that seam answers (a route missing a `@required` http-bound member, which `tests/all/portal_session.rs:960-975` pins as a 400). Folding the block into one helper in `crate::state` — the module that already owns every rule the seams and handlers share — leaves both copies' behaviour and every existing test exactly as they are, and leaves one place to change when the rule changes.

## Evidence

The crate's own stated principle — `services/service-com-amazonaws-sso-swbportalservice/src/state.rs:23-24`:
```
//! This module also owns the rules every handler shares, so none is written twice:
```

Copy 1 — `src/service/session_gate.rs:150-162`:
```rust
            // The one write every portal request makes, and only what the clock already
            // owed: a minted role credential past its advertised expiration is spent at
            // that instant, whether or not the request that happens to arrive next is one
            // this service will serve, so it is ended here exactly as each handler ends
            // it. Nothing about the refusal below changes because this ran.
            if let Some(state) = crate::state::service_state(&*self.engine) {
                let mut state = state.lock();
                crate::state::end_expired_credentials(
                    &mut state,
                    ctx.credential_store(),
                    SystemTime::now(),
                );
            }
```

Copy 2 — `src/service/unreadable_request.rs:109-122`, the same block verbatim:
```rust
            // The one write every portal request makes, and only what the clock already
            // owed: a minted role credential past its advertised expiration is spent at
            // that instant, whether or not the request that happens to arrive next is one
            // this service will serve, so it is ended here exactly as the authentication
            // gate and every handler end it. Nothing about the refusal below changes
            // because this ran.
            if let Some(state) = crate::state::service_state(&*self.engine) {
                let mut state = state.lock();
                crate::state::end_expired_credentials(
                    &mut state,
                    ctx.credential_store(),
                    SystemTime::now(),
                );
            }
```

The drift the two copies already show — `src/service.rs:457-515` (`override_routing_error`) answers a portal request through the same engine and never sweeps:
```rust
        let absent = absent_required_members(req)?;
        ...
        match self.session_gate.presented_session(req) {
            PresentedSession::NoUsableSession => {
                return crate::state::unauthorized_response(self.entry.protocol())
            }
```
(no `end_expired_credentials` anywhere in the method, although both copies above call it "the one write every portal request makes").

Test coverage that must stay green and unchanged: `tests/all/credential_expiry.rs:153-158` ("RULE: a minted role credential ends at the expiration its response advertised", whose `Next::ListAccounts(UNKNOWN)` row at `:198-203` is answered through the session-gate seam that holds copy 1), `tests/all/unreadable_wire_value.rs:7-14` (the seam that holds copy 2), and `tests/all/bearer_token_gate.rs:298`.

## Steps to Reproduce

1. Open `services/service-com-amazonaws-sso-swbportalservice/src/service/session_gate.rs` and read lines 150-162.
2. Open `services/service-com-amazonaws-sso-swbportalservice/src/service/unreadable_request.rs` and read lines 109-122: the same `service_state` → `lock` → `end_expired_credentials(.., ctx.credential_store(), SystemTime::now())` block, with the same comment.
3. Run `grep -n 'end_expired_credentials' services/service-com-amazonaws-sso-swbportalservice/src` — the rule's call site is written six times (four handlers, which already hold the lock for their own work, plus these two seams, which hold it for nothing else).
4. Read `src/service.rs:457-515`: a third seam that answers a portal request without a handler and carries no copy of the block, which is the drift two copies invite.
5. Fix: give `crate::state` one function that performs the seam's sweep (reach the store, lock, call `end_expired_credentials`) and call it from both seams. Every response and every test in `tests/all/` stays byte-identical.
