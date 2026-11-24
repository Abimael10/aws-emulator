Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: f94f81a4e4acb5f06aec5c20cf2f41781bd39454

# Connection recording preferences are stored globally, not per-Region, so an Update in one Region overwrites another Region's configuration

_Filed: 2026-11-24T12:36:39Z_
_Operation: UpdateConnectionRecordingPreferences | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `f94f81a4e4acb5f06aec5c20cf2f41781bd39454`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every doc in this crate — and the Smithy operation doc — describes the stored resource as the *per-account/per-Region* connection recording preferences singleton, and the crate's two gates genuinely key on the Region the request's SigV4 credential scope names. The STORE does not: `ComAmazonawsSsmguiconnectSsmguiconnectState` holds one bare `Option<RecordingPrefsRecord>` with no Region in the key and no Region member on the record, and none of the three handlers ever reads a Region before touching it.

Consequences, all observable on the wire:

1. `UpdateConnectionRecordingPreferences` signed for `us-west-2` REPLACES the record an earlier Update signed for `us-east-1` persisted — the first Region's configuration is lost even though that Region's enrollment and that Region's IAM grant were what authorized it.
2. `GetConnectionRecordingPreferences` signed for a Region that was never configured returns the record written in a *different* Region, instead of the `ResourceNotFoundException` the handler's own doc reserves for "nothing configured".
3. `DeleteConnectionRecordingPreferences` signed for one Region removes the singleton for all of them.

The crate already owns the helper it needs for this (`state::authorization::request_region`) and already uses the same reading twice — once for the IAM resource ARN and once for the just-in-time node access gate — so the store path is the one place in this crate that needs the Region and does not ask for it. That is also why the inconsistency is visible: the enrollment gate is decided per Region, so an account enrolled only in `us-west-2` can mutate (and destroy) the configuration the `us-east-1` Region holds.

## Evidence

Smithy model text for the operation (`smithy_query(query="digest")` / `OPERATIONS.md:25-31`), which states the behaviour itself:

  "Returns the preferences specified for recording RDP connections in the requesting Amazon Web Services account and Amazon Web Services Region."

and for the shape (`smithy_query(query="shape", shape="ConnectionRecordingPreferences")`):

  "The set of preferences used for recording RDP connections in the requesting Amazon Web Services account and Amazon Web Services Region."

The crate's own claim, beside the field that breaks it — `services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state.rs:73-76`:

```
    /// The one per-account/per-Region connection recording preferences record.
    /// `None` means never configured (or deleted), which every read reports as
    /// `ResourceNotFoundException`.
    connection_recording_preferences: Option<RecordingPrefsRecord>,
```

`RecordingPrefsRecord` carries no Region either — `src/state.rs:93-106`: `kms_key_arn`, `s3_buckets`, `client_token`, `last_updated`.

The sibling path in the SAME crate that does key on the Region — `src/state/just_in_time_node_access.rs:24-28`:

```
//! The decision is taken per REGION, because an approval policy is a regional document and
//! the recording preferences are themselves a per-account/per-Region singleton: the Region
//! is the one the request's SigV4 credential scope names, read through Systems Manager's own
//! reading of it ...
```

and `src/state/just_in_time_node_access.rs:46`: `let region = ssm_store::request_region(parts);`

The helper this crate already owns for the same reading — `src/state/authorization.rs:131-134`:

```
pub fn request_region(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map_or_else(|| DEFAULT_REGION.to_string(), |scope| scope.region)
}
```

The handler paths that skip it:
* `src/ops/update_connection_recording_preferences.rs:243` `let mut store = state.lock();` then `:265` `store.put_preferences(record.clone());` — no Region is read anywhere in the handler except inside the two gates.
* `src/ops/get_connection_recording_preferences.rs:179` `let stored = state.lock().preferences().cloned();` — no Region.
* `src/ops/delete_connection_recording_preferences.rs:209` `let removed = store.remove_preferences(&client_token);` — no Region.

And the store the handlers reach is a single process-wide instance keyed only by Rust type — `vendor/engine/src/lib.rs:139-147`: `map.entry(TypeId::of::<T>())`.

Test coverage: `tests/all/jit_node_access_gate_and_error_messages.rs:589-650` (`the_enrollment_is_decided_for_the_region_the_request_was_signed_for`) already drives Updates signed for `us-east-1`, `us-west-2` and `eu-west-3` against ONE store, and asserts only the 200/403 of the gate — it never asserts that the record each Region holds is its own, so no test pins the global behaviour.

## Steps to Reproduce

1. Build a router over one engine with this service plus Systems Manager (as `tests/all/jit_node_access_gate_and_error_messages.rs::router_with_engine` does).
2. Enroll the account in just-in-time node access in `us-east-1` and in `us-west-2` (create the access-request approval policy in each Region through SSM `CreateDocument`).
3. Call `UpdateConnectionRecordingPreferences` signed for `us-east-1` with `BucketName="east-recordings"`, `BucketOwner="123456789012"`, `ClientToken="east"` — 200.
4. Call `UpdateConnectionRecordingPreferences` signed for `us-west-2` with `BucketName="west-recordings"`, `ClientToken="west"` — 200.
5. Call `GetConnectionRecordingPreferences` signed for `us-east-1`. Expected per the modelled sentence ("the preferences specified ... in the requesting ... Region"): the `east-recordings` record. Actual: `west-recordings` — the us-east-1 Region's configuration was destroyed by a write signed for another Region.
6. Call `DeleteConnectionRecordingPreferences` signed for `us-west-2`, then `GetConnectionRecordingPreferences` signed for `us-east-1`. Expected: us-east-1 still reports its own record. Actual: 404 `ResourceNotFoundException` for every Region.
7. Also: on a store where only `us-east-1` was ever configured, call `GetConnectionRecordingPreferences` signed for `eu-west-3`. Expected: 404 (nothing configured in that Region). Actual: 200 with the us-east-1 record.
