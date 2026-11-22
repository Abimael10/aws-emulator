Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: e6fc59220a21643213ef781c20ea38f623e54ae0

# The TtlDuration.Unit enum guard is implemented three times in this crate, while the crate's sibling enum guards are stated once

_Filed: 2026-11-22T16:07:17Z_
_Operation: BatchWriteRecord | Class: maintainability_

> _Note: this issue was identified against the code base at commit `e6fc59220a21643213ef781c20ea38f623e54ae0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate centralises every other "a wire member carrying an enum arrived with a value the model does not declare" guard in one place in `crate::state`: `undeclared_target_store` (whose doc says it is "stated once so no two of them word it differently") and `expiration_time_enabled` ("Shared by GetRecord and BatchGetRecord, which both carry the member, so the two cannot drift"). The `TtlDuration.Unit` guard is the one that was never given that treatment: the same branch (`ttl_seconds(ttl)` is `None` -> refuse) and the same hand-copied refusal message literal exist three times — inside `crate::state::prepare_record` (the copy every whole-record write reaches, i.e. PutRecord and each BatchWriteRecord entry), inside the BatchWriteRecord handler for the request-level `TtlDuration`, and inside the UpdateRecord handler for its own `TtlDuration`. BatchWriteRecord alone reaches two of the three copies in a single request. The copies can drift: a change to the wording or to the value list in one leaves the other two stating the old rule, which is exactly the drift the sibling guards were centralised to prevent. All three produce byte-identical text today (same format string, same `TTL_DURATION_UNIT_VALUES.join(", ")`), so replacing them with one `crate::state`-level statement (e.g. an `undeclared_ttl_unit(&TtlDuration) -> Option<String>` beside `undeclared_target_store`) leaves every response and every existing test unchanged.

## Evidence

Copy 1 — `services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/state.rs:508-520` (inside `prepare_record`, reached by PutRecord and by every BatchWriteRecord entry):
```
    let record_ttl_secs = match ttl_duration {
        None => None,
        Some(ttl) => match ttl_seconds(ttl) {
            Some(secs) => Some(secs),
            None => {
                return Err(format!(
                    "TtlDuration.Unit: '{}' is not a TtlDurationUnit; expected one of {}",
                    ttl.unit,
                    crate::ops::types::TTL_DURATION_UNIT_VALUES.join(", ")
                ))
            }
        },
    };
```
Copy 2 — `services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/ops/batch_write_record.rs:196-204`:
```
        if let Some(ttl) = input.ttl_duration.as_ref() {
            if crate::state::ttl_seconds(ttl).is_none() {
                return Err(validation(format!(
                    "TtlDuration.Unit: '{}' is not a TtlDurationUnit; expected one of {}",
                    ttl.unit,
                    TTL_DURATION_UNIT_VALUES.join(", ")
                )));
            }
        }
```
Copy 3 — `services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/ops/update_record.rs:326-338`:
```
        let ttl_secs = match input.ttl_duration.as_ref() {
            None => None,
            Some(ttl) => match crate::state::ttl_seconds(ttl) {
                Some(secs) => Some(secs),
                None => {
                    return Err(validation(format!(
                        "TtlDuration.Unit: '{}' is not a TtlDurationUnit; expected one of {}",
                        ttl.unit,
                        TTL_DURATION_UNIT_VALUES.join(", ")
                    )))
                }
            },
        };
```
The convention these three depart from, `.../src/state.rs:704-719`:
```
/// Every write of this data plane reads the list the same way — "is the OnlineStore
/// among these?" ([`targets_online_store`]) — so an undeclared value would be read as
/// "not the OnlineStore" and silently turn the write into a no-op success. The boundary
/// validator refuses it first; this is the handlers' own guard, stated once so no two of
/// them word it differently.
pub fn undeclared_target_store(target_stores: Option<&Vec<String>>) -> Option<String> {
```
and `.../src/state.rs:763-783` for the other enum member: "Shared by GetRecord and BatchGetRecord, which both carry the member, so the two cannot drift."

Test coverage that pins the behaviour the de-duplication must not change: `tests/all/batch_write_record_bulk_ingestion.rs:1080-1114` (`a_request_level_ttl_duration_in_no_declared_unit_refuses_the_whole_request`, asserting HTTP 400 and the error code only), `tests/all/update_record_semantics.rs:497-569` and `tests/all/online_store_record_lifecycle.rs:470` / `tests/all/feature_group_default_ttl.rs:199-201` (every declared unit converts and sets `ExpiresAt`). None of them asserts the refusal wording, and the three copies emit identical text anyway, so responses and tests stay unchanged.

## Steps to Reproduce

1. Read `src/state.rs:704-719`: the crate states its `TargetStores` enum guard once, explicitly "so no two of them word it differently", and `src/state.rs:763-783` does the same for `ExpirationTimeResponse`.
2. Read `src/state.rs:508-520`, `src/ops/batch_write_record.rs:196-204` and `src/ops/update_record.rs:326-338`: the `TtlDuration.Unit` guard — same branch, same message literal — written out three times instead.
3. Note that one BatchWriteRecord request reaches two of those copies: the handler's own copy for the request-level `TtlDuration` (line 196) and `prepare_record`'s copy for each entry's `TtlDuration` (line 508, via line 280).
4. Add one `crate::state` helper beside `undeclared_target_store` that returns the same message for an undeclared unit, call it from all three sites, and re-run `tests/all/batch_write_record_bulk_ingestion.rs` and `tests/all/update_record_semantics.rs`: every response is byte-identical.
