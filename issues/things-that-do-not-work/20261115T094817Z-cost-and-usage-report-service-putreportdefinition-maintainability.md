Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 5d93654b0bd14cd350333b75279398fadc574ee6

# Build the Parquet-preference refusal in one constructor like this crate's other shared refusals

_Filed: 2026-11-15T09:48:17Z_
_Operation: PutReportDefinition | Class: maintainability_

> _Note: this issue was identified against the code base at commit `5d93654b0bd14cd350333b75279398fadc574ee6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every refusal this crate answers from TWO handlers is built by a single constructor in `crate::state`, and each of those constructors documents that as the reason it exists. The Parquet-preference refusal is the one exception: only the message CONSTANT is shared, while the error-shape construction (`ValidationException { message: Some(crate::state::PARQUET_PREFERENCE_MESSAGE.to_string()) }`) is written out inline in both `PutReportDefinition` and `ModifyReportDefinition`. The two copies are the same rule — which modelled error shape this condition is reported under, and with which message member — implemented twice, so one site can be changed (a different shape, a message built differently, a `None` message) without the other, which is exactly the drift the sibling constructors were introduced to prevent. The fix is to add one constructor beside `report_not_found`/`report_name_already_held` in `src/state.rs` and have both handlers call it; the wire bytes and every existing test stay unchanged.

## Evidence

Copy 1 — services/.../src/ops/put_report_definition.rs:192-199:
```
        if !crate::state::parquet_preference_is_usable(&input.report_definition) {
            return Err(PutReportDefinitionErrorModeled::ValidationException(
                ValidationException {
                    message: Some(crate::state::PARQUET_PREFERENCE_MESSAGE.to_string()),
                },
            )
            .into());
        }
```
Copy 2 — services/.../src/ops/modify_report_definition.rs:188-195:
```
        if !crate::state::parquet_preference_is_usable(&input.report_definition) {
            return Err(ModifyReportDefinitionErrorModeled::ValidationException(
                ValidationException {
                    message: Some(crate::state::PARQUET_PREFERENCE_MESSAGE.to_string()),
                },
            )
            .into());
        }
```
The crate's own stated convention for exactly this situation — services/.../src/state.rs:52-53: "One constructor so the handlers that answer this refusal cannot drift apart." (on `report_not_found`); src/state.rs:71-72: "One constructor, so the create and the update path cannot drift apart about the one condition they share." (on `report_name_already_held`); src/state.rs:240-241: "The fixed text and the error shape live here alone, so the handlers that answer this refusal cannot drift apart." (on `TagLimitExceeded::refusal`). The shared predicate already lives in one place (src/state.rs:184 `pub fn parquet_preference_is_usable`), and src/state.rs:182-183 says it is "Shared by the two operations that take a `ReportDefinition`, so the create and the update cannot drift apart" — only the refusal construction was left duplicated.

Test coverage that pins the responses both sites must keep producing: services/.../tests/all/service_populated_read_back.rs:284 `a_definition_whose_parquet_preference_is_not_usable_is_refused`, which asserts `(400, "ValidationException")` and the exact `Message` through PutReportDefinition (lines 386-398) and through ModifyReportDefinition (lines 430-441).

## Steps to Reproduce

1. Open services/service-com-amazonaws-costandusagereportservice-awsorigamiservicegatewayservice/src/ops/put_report_definition.rs at lines 192-199 and src/ops/modify_report_definition.rs at lines 188-195: the same `ValidationException { message: Some(crate::state::PARQUET_PREFERENCE_MESSAGE.to_string()) }` refusal is constructed in both.
2. Compare with src/state.rs:54 (`report_not_found`), :73 (`report_name_already_held`) and :236 (`TagLimitExceeded::refusal`), each of which exists so the two handlers answering the same condition cannot drift apart.
3. Change the message or the error shape at only one of the two Parquet sites: the crate still compiles and only one operation's refusal changes, while tests/all/service_populated_read_back.rs:284 asserts both paths must answer identically.
