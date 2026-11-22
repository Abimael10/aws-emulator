Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: b9c2497089b8fff0fb7befac08abb06355746155

# The entitlements an environment's deletion stops in place can never be read back: the only reader requires a live connector, and the same deletion makes every connector non-live

_Filed: 2026-11-22T20:53:27Z_
_Operation: ListVmEntitlements | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `b9c2497089b8fff0fb7befac08abb06355746155`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When `DeleteEnvironment` is accepted, the crate deliberately STOPS each entitlement of the environment IN PLACE rather than removing the record, and the state module states the reason for that choice: the records "stay in the store so a later read still reports the stop". No later read can report them.

`ListVmEntitlements` is the only operation that reports an entitlement, and it refuses any `connectorId` the environment holds no LIVE connector for with `ResourceNotFoundException`. The very same `start_dependent_deletions` call that stops the entitlements also moves EVERY live connector of the environment to `DELETING`, and `StoredConnector::is_live` — the predicate `live_connector` resolves records by — excludes `DELETING`. So from the instant the stop is written, every `connectorId` of that environment resolves to nothing and `ListVmEntitlements` answers 404 for all of them; after the deletion settles, the connectors are `DELETED` and nothing changes.

The result is that the stopped records are kept for a read that cannot happen: a client that deletes an environment and then lists its entitlements is told the connector does not exist instead of being handed the `DELETED`/`stoppedAt` records the crate says it retains for exactly that purpose. Either the retention has no effect, or the reader should resolve the connector of a stopped entitlement; as written the two halves of the crate contradict each other.

## Evidence

The retention and its stated purpose — `src/state.rs:1102-1105`:
```
/// Stop usage tracking for this record IN PLACE, the form used where the record is
/// KEPT rather than removed — the entitlements a deletion of the whole environment
/// stops, which stay in the store so a later read still reports the stop.
pub fn stop(&mut self, stopped_at_ms: i64) {
```

The same call that stops them also tears down every connector — `src/state.rs:1753-1776` (invoked from `src/ops/delete_environment.rs:289`, `environment.start_dependent_deletions(&deletion, accepted_at_ms);`):
```
pub fn start_dependent_deletions(&mut self, deletion: &PendingTransition, now_ms: i64) {
    for connector in self.connectors.values_mut() {
        if !connector.is_live() { continue; }
        connector.state = CONNECTOR_DELETING.to_string();
...
    for entitlement in self.entitlements.values_mut() {
...
        entitlement.stop(now_ms);
```

A `DELETING` connector is not live — `src/state.rs:901-903`:
```
pub fn is_live(&self) -> bool {
    self.state != CONNECTOR_DELETING && self.state != CONNECTOR_DELETED
}
```

And the only reader of an entitlement refuses on exactly that predicate — `src/ops/list_vm_entitlements.rs:246-251`:
```
if crate::state::live_connector(environment, &input.connector_id).is_none() {
    return Err(ListVmEntitlementsErrorModeled::ResourceNotFoundException(
        crate::state::connector_not_found(&input.connector_id),
    )
    .into());
}
```

The list is NOT stopped earlier by the lifecycle gate: `src/state.rs:629-634` states that only `STATE_CREATING` is refused and "Every other `EnvironmentState` the model declares — `CREATED`, `DELETING`, `DELETED`, `CREATE_FAILED` … is past initialization … so a list against one of those is answered". So the request reaches the connector check and is refused there.

Test coverage: `tests/all/environment_deletion_cascade.rs` and `tests/all/entitlements.rs` exercise the cascade and the entitlement reads, but no test asserts that a stopped entitlement of a deleted environment is ever reported — which is the read the quoted comment promises.

## Steps to Reproduce

1. Create an environment and let it settle to CREATED.
2. Create a VCENTER connector in it and let it settle to ACTIVE.
3. CreateEntitlement with that connectorId and a vmIds list, so the environment holds a tracking entitlement.
4. ListVmEntitlements with the same environmentId/connectorId/entitlementType — 200, the entitlement is reported.
5. Delete every host (if any) and call DeleteEnvironment; it is accepted, the environment is DELETING, `start_dependent_deletions` stops the entitlement in place (status DELETED, stoppedAt set) and moves the connector to DELETING.
6. Call ListVmEntitlements again with the same three members: the response is ResourceNotFoundException ("Connector <id> was not found") instead of the stopped entitlement record the state module says is kept "so a later read still reports the stop". The same holds after the deletion settles.
