Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: ed5ba69ed7ffa6ceed26f40db456a183b331c608

# The "hand a connection to the clock in a transient provisioning state" write is hand-rolled in DeleteConnection instead of the state-module writer the sibling path uses

_Filed: 2026-11-23T19:36:38Z_
_Operation: DeleteConnection | Class: maintainability_

> _Note: this issue was identified against the code base at commit `ed5ba69ed7ffa6ceed26f40db456a183b331c608`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate's convention is that every write that puts a connection into a transient state and hands its successor to the clock lives in `state.rs` as a NAMED single writer: `crate::state::owed_progression` for the create/clock legs, `crate::state::apply_acceptance` ("the ONE writer of everything an acceptance decides"), and `crate::state::apply_bandwidth_change` ("the ONE writer of what a re-provisioning change does to the record"). `DeleteConnection` is the one handler that does not: it assembles the same computation inline in its own body — set the transient state, then `pending_transition = Some(PendingTransition { due: now + PROVISIONING_SPAN, state: <successor> })`.

So the identical rule — "enter the transient state now, and the clock settles into the successor one `PROVISIONING_SPAN` later" — exists as two copies in this crate: `crate::state::apply_bandwidth_change` (state.rs) and the body of `delete_connection.rs`. `delete_connection.rs:177-180` is also the ONLY construction of `crate::state::PendingTransition` and the ONLY reader of `crate::state::PROVISIONING_SPAN` outside `state.rs` (grep over the crate finds exactly these two sites for both symbols), so the handler copy is the one place the rule can drift from the module that otherwise owns it — e.g. one copy re-armed from `pending.due` instead of `now`, or one copy gaining the `evict_due` bookkeeping the other needs.

The defect is the duplication only. Folding the delete's write into a sibling state-module writer (e.g. `crate::state::apply_deletion(stored, now)`, beside `apply_bandwidth_change` and `apply_acceptance`) leaves every response byte-identical: the same two fields get the same two values, the `@idempotent` guard that skips the write stays in the handler where the verdict is made, and no existing test observes anything but the resulting `state` and the clock-settled successor.

## Evidence

Copy 1 — hand-rolled in the handler, services/service-com-amazonaws-interconnect-interconnect/src/ops/delete_connection.rs:168-181:
```
        // "deleting: the connection is being deleted" -> "deleted: the connection has
        // been deleted". The call starts the first and the clock owes the second, so
        // a caller reading straight back still sees `deleting`.
        //
        // @idempotent: a connection already in one of those two states is left
        // exactly as it is, and the same record is returned — a retried delete
        // neither restarts the transition nor fails.
        if !crate::state::is_deleted_or_deleting(&stored.connection.state) {
            stored.connection.state = crate::state::DELETING_CONNECTION_STATE.to_string();
            stored.pending_transition = Some(crate::state::PendingTransition {
                due: now + crate::state::PROVISIONING_SPAN,
                state: crate::state::DELETED_CONNECTION_STATE.to_string(),
            });
        }
```

Copy 2 — the same computation as a named state-module writer, services/service-com-amazonaws-interconnect-interconnect/src/state.rs:1853-1874:
```
/// Apply a new bandwidth to a connection: the ONE writer of what a re-provisioning
/// change does to the record.
...
pub fn apply_bandwidth_change(stored: &mut StoredConnection, bandwidth: String, now: Instant) {
    let came_from = stored.connection.state.clone();
    stored.connection.bandwidth = bandwidth;
    stored.connection.state = UPDATING_CONNECTION_STATE.to_string();
    stored.pending_transition = Some(PendingTransition {
        due: now + PROVISIONING_SPAN,
        state: came_from,
    });
}
```

The convention the delete is the exception to — services/service-com-amazonaws-interconnect-interconnect/src/state.rs:1800-1801:
```
/// Accept a connection's proposal: the ONE writer of everything an acceptance decides.
```
and services/service-com-amazonaws-interconnect-interconnect/src/state.rs:151-154:
```
/// Derived from the state alone, so EVERY path that leaves a connection in a
/// provisioning state hands it on along the same progression — the create that
/// allocates it, and the clock leg or settled update that lands it there.
pub fn owed_progression(state: &str, entered: Instant) -> Option<PendingTransition> {
```

Every other state handoff in the crate goes through those writers — create_connection.rs:573 `pending_transition: crate::state::owed_progression(&connection.state, now)`, state.rs:1850 `stored.pending_transition = owed_progression(ACCEPTED_CONNECTION_STATE, now)`, state.rs:408 `stored.pending_transition = owed_progression(&stored.connection.state, pending.due)` — so delete_connection.rs:177 is the only site that builds the struct by hand.

Test coverage that must stay unchanged: tests/all/connection_lifecycle.rs:1330-1407 (`delete_connection_moves_through_deleting_to_deleted`, asserting `state == "deleting"` on the call and the clock-settled `deleted` after it, plus the `("deleting", false)` refusal row), tests/all/connection_state_progression.rs:259 (the `("deleting", false)` row), tests/all/connection_proposal.rs:338,418 (a proposal whose connection is `deleting` with the clock owing `deleted`), tests/all/attach_point_arn_stops_at_the_delete.rs, and tests/all/update_settles_back_into_the_state_it_came_from.rs (which pins the `apply_bandwidth_change` copy).

## Steps to Reproduce

1. Read services/service-com-amazonaws-interconnect-interconnect/src/ops/delete_connection.rs:175-181 and note the inline `stored.connection.state = <transient>; stored.pending_transition = Some(PendingTransition { due: now + PROVISIONING_SPAN, state: <successor> })`.
2. Read services/service-com-amazonaws-interconnect-interconnect/src/state.rs:1866-1874 (`apply_bandwidth_change`) and note the identical two-field computation, there behind a doc that calls itself "the ONE writer".
3. grep the crate for `PendingTransition` and `PROVISIONING_SPAN`: the only matches outside src/state.rs are src/ops/delete_connection.rs:177-178, confirming the delete is the sole handler that constructs the transition itself while every other state handoff calls a state-module writer (`owed_progression` at create_connection.rs:573 and state.rs:408,1850; `apply_acceptance` at state.rs:1822; `apply_bandwidth_change` at state.rs:1866).
4. Confirm the duplication is the whole defect: run tests/all/connection_lifecycle.rs::delete_connection_moves_through_deleting_to_deleted and tests/all/update_settles_back_into_the_state_it_came_from.rs — both pass today, and both must still pass unchanged after the delete's write is moved into a single state-module writer beside its two siblings.
