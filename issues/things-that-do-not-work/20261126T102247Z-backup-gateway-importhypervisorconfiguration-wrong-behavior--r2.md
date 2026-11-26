Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9c31bf6152f020b6d0683107981107b2a4f16ac3

# state.rs says the import handler "resolves the host" before minting, while the handler states it does no lookup at all and two other state.rs sentences forbid one

_Filed: 2026-11-26T10:22:47Z_
_Operation: ImportHypervisorConfiguration | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9c31bf6152f020b6d0683107981107b2a4f16ac3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ComAmazonawsBackupgatewayBackuponpremisesV20210101State::import_hypervisor`'s own doc describes the shape of the `ImportHypervisorConfiguration` handler that calls it: "its handler resolves the host, bounds the store and then calls here". The handler does nothing of the sort, deliberately: it bounds the store and inserts, and its comment states in so many words that there is NO lookup to do first because `Host` is not a key.

The code is the half this crate pins. `hypervisors_for_host`'s own doc states the opposite rule for the whole store ("A lookup, never a constraint — no writer of this store consults it to decide whether a record may exist"), the store module doc states it again for hypervisor identity ("any number of records may hold the same host ... and no import or update is refused because another record already carries the value it asks for"), the only caller of `hypervisors_for_host` in `src/` is `TestHypervisorConfiguration`, and `tests/all/hypervisor_host_is_not_a_key.rs` plus `tests/all/hypervisor_import_is_not_deduplicated.rs` pin that an import mints a record per call whatever host another record already holds.

So the defect is the stale sentence, which is a leftover from the behaviour a FIXED issue removed (`issues/.../20261023T110543Z-backup-gateway-importhypervisorconfiguration-wrongly-enforces-host-uniqueness-and-returns-a.md`: "ImportHypervisorConfiguration wrongly enforces Host uniqueness and returns a spurious ConflictException"). It sits directly above the one function the import handler writes through, so a reader who trusts it would conclude the handler is missing the host lookup the store expects and re-add it — re-introducing exactly the refusal that fix removed and breaking the two suites above. The sentence should describe what the handler really does (validate the request's tag pairs, bound the store, and insert without consulting any other record).

## Evidence

The stale rule, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:2173-2175`:
```
    /// `ImportHypervisorConfiguration` is the operation that performs this: its handler
    /// resolves the host, bounds the store and then calls here, so the record a caller
    /// gets an ARN for is the one this map holds.
```
immediately above `pub fn import_hypervisor(` at `src/state.rs:2176`.

The handler under it says the opposite — `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/import_hypervisor_configuration.rs:181-186`:
```
        // One guard spans the ceiling check and the insert, so two concurrent imports
        // cannot both pass a check only one of them should. There is no lookup to do
        // first: the model declares no uniqueness constraint and no idempotency token on
        // any input member, so every accepted import mints its OWN record under its own
        // freshly minted ARN, whatever `Name` or `Host` another record already carries.
        let mut store = state.lock();
```
and the whole `handle` body (`:147-232`) contains exactly two store reads — `store.hypervisor_count()` at `:188` and `store.import_hypervisor(...)` at `:213` — and no host lookup.

The same rule stated against the host lookup itself, `src/state.rs:2266-2271`:
```
    /// A hypervisor is keyed by the ARN minted for it and by nothing else: `Host` is
    /// metadata the record carries, not an identity, so any number of records may hold one
    /// host and this hands back all of them. A lookup, never a constraint — no writer of
    /// this store consults it to decide whether a record may exist.
    pub fn hypervisors_for_host(&self, host: &str) -> Vec<&StoredHypervisor> {
```
and again in the store's module doc, `src/state.rs:46-49`:
```
//! The ARN minted for a record is its ONLY identity: `Host`, `Name` and the rest are
//! metadata the record carries, so any number of records may hold the same host (one
//! appliance re-registered, or configured several times over) and no import or update is
//! refused because another record already carries the value it asks for.
```

`grep -n hypervisors_for_host src/` returns only the definition (`src/state.rs:2272`) and `src/ops/test_hypervisor_configuration.rs:13` / `:233` — no import path reaches it.

Existing coverage that pins the handler's behaviour and contradicts the doc — `tests/all/hypervisor_host_is_not_a_key.rs:8-13`:
```
//! The rule under test: a hypervisor is identified by the `HypervisorArn` minted for it
//! and by nothing else, so `Host` is metadata the record carries rather than a key — any
//! number of records may hold one host, and no call is refused because another record
//! already carries the host it asks for. That holds for whichever path writes the host:
//! `ImportHypervisorConfiguration` mints a record per call, and `UpdateHypervisor` writes
//! the host its request names onto the record its ARN names.
```
plus `tests/all/hypervisor_import_is_not_deduplicated.rs`.

## Steps to Reproduce

1. Read `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:2173-2175`: the doc of `import_hypervisor` states that the `ImportHypervisorConfiguration` handler "resolves the host" before calling it.
2. Read `src/ops/import_hypervisor_configuration.rs:147-232`: the handler validates the request's `Tags` (`:167`), reads `store.hypervisor_count()` (`:188`) and calls `store.import_hypervisor(...)` (`:213`) — and its comment at `:181-185` states "There is no lookup to do first".
3. Confirm no import path can resolve a host: `grep -n 'hypervisors_for_host' src/` gives only `src/state.rs:2272` (the definition) and `src/ops/test_hypervisor_configuration.rs:13`/`:233`.
4. Read `src/state.rs:2266-2271` and `src/state.rs:46-49`: both state that `Host` is never a key and that no writer consults the host lookup to decide whether a record may exist — the rule the stale sentence contradicts.
5. Note the behaviour is pinned: `tests/all/hypervisor_host_is_not_a_key.rs:8-13` and `tests/all/hypervisor_import_is_not_deduplicated.rs` assert one record per import whatever host another record holds, which is what the FIXED issue `20261023T110543Z-backup-gateway-importhypervisorconfiguration-wrongly-enforces-host-uniqueness-and-returns-a.md` established — so `state.rs:2173-2175` is the stale side and is what should be reworded.
