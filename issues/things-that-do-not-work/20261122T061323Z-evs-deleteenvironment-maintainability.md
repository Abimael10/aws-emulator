Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: b20300340ddfd95fa2f862f6c06bba5164a73eba

# The "stop every entitlement that still tracks" rule is written out three times — twice verbatim — instead of through the predicate live_entitlements single-homes

_Filed: 2026-11-22T06:13:23Z_
_Operation: DeleteEnvironment | Class: maintainability_

> _Note: this issue was identified against the code base at commit `b20300340ddfd95fa2f862f6c06bba5164a73eba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The predicate "this entitlement still TRACKS a virtual machine's usage" (`status` has reached neither `ENTITLEMENT_DELETED` nor `ENTITLEMENT_REMOVED`) is single-homed by `StoredEnvironment::live_entitlements`, whose doc names it as the one reading of that line. The two halves of the environment-deletion cascade then re-spell the same predicate inline, in two byte-identical loops that differ only in the instant passed to `StoredEntitlement::stop`.

This is the same shape as the already-FIXED `DeleteEnvironmentConnector` maintainability issue (20261121T094725Z), where the "this connector is already going away" predicate was spelled out instead of calling the `StoredConnector::is_live` the crate single-homes — and note the connector loop in the very same `start_dependent_deletions` DOES call `connector.is_live()` (state.rs:1758), so the two record kinds in one function are handled by two different conventions. A future change to what counts as a stopped entitlement (for example, anything that writes `ENTITLEMENT_REMOVED`, which the crate notes nothing writes yet) has to be made in three places and the copies can drift.

The fix is a pure refactor: give `StoredEntitlement` a `tracks_usage()` (or equivalent) predicate, have `live_entitlements` filter on it, and have both cascade loops `continue` on its negation. No response and no existing test changes.

## Evidence

The single-homed predicate — `services/.../src/state.rs:1984-1996`:
```
    /// The entitlements of this environment that still TRACK a virtual machine's usage:
    /// every stored entitlement whose `EntitlementStatus` has reached neither
    /// [`ENTITLEMENT_DELETED`] nor [`ENTITLEMENT_REMOVED`].
    pub fn live_entitlements(&self) -> impl Iterator<Item = &StoredEntitlement> {
        self.entitlements.values().filter(|entitlement| {
            entitlement.status != ENTITLEMENT_DELETED && entitlement.status != ENTITLEMENT_REMOVED
        })
    }
```
Copy 1 — `services/.../src/state.rs:1769-1776` (`start_dependent_deletions`, the call `DeleteEnvironment` makes at ops/delete_environment.rs:289):
```
        for entitlement in self.entitlements.values_mut() {
            if entitlement.status == ENTITLEMENT_DELETED
                || entitlement.status == ENTITLEMENT_REMOVED
            {
                continue;
            }
            entitlement.stop(now_ms);
        }
```
Copy 2 — `services/.../src/state.rs:1796-1803` (`finish_dependent_deletions`):
```
        for entitlement in self.entitlements.values_mut() {
            if entitlement.status == ENTITLEMENT_DELETED
                || entitlement.status == ENTITLEMENT_REMOVED
            {
                continue;
            }
            entitlement.stop(settled_at_ms);
        }
```
The sibling convention in the SAME function, which does call the single-homed predicate — `services/.../src/state.rs:1754-1760`:
```
        for connector in self.connectors.values_mut() {
            // The same one predicate every reader and writer of a connector resolves
            // records by ([`StoredConnector::is_live`]): a record whose teardown has
            // already started is left exactly as it stands.
            if !connector.is_live() {
                continue;
            }
```
Tests that cover both cascade halves and must stay unchanged: `tests/all/environment_deletion_cascade.rs`, `tests/all/entitlement_stop_stamp.rs`, `tests/all/entitlements.rs`, `tests/all/delete_entitlement.rs`, and `tests/all/state_roundtrip.rs`.

## Steps to Reproduce

1. Open `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state.rs` and read `live_entitlements` (lines 1984-1996): the "still tracks usage" predicate is stated there as the one reading of that line.
2. Read `start_dependent_deletions` (lines 1769-1776) and `finish_dependent_deletions` (lines 1796-1803): the same predicate is re-spelled inline in two byte-identical guards.
3. Note that the connector loop immediately above the first copy (lines 1754-1760) calls `connector.is_live()` rather than re-spelling it, so one function applies two conventions to its two record kinds.
4. A change to what counts as a stopped entitlement must now be made in three places; the copies can drift silently.
