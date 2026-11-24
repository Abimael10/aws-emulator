Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 05c7972ccec5de40063e90508b79406e82288e9e

# The "unresolvable environmentId" verdict is mapped to a refusal three times, once per environment-resolving handler, although the crate already shares such mappings through a refusal trait

_Filed: 2026-11-24T18:56:41Z_
_Operation: GetEnvironment | Class: maintainability_

> _Note: this issue was identified against the code base at commit `05c7972ccec5de40063e90508b79406e82288e9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Three handlers of this crate resolve an `environmentId` (or `GetEnvironment$id`) in the called Region's catalog and, when the catalog holds no such record, classify and word the refusal themselves: `GetEnvironment`, `ListAttachPoints` and `CreateConnection` each spell out the same branch — `environment_in_region(...)` returning `None` => `InterconnectClientException { message: environment_not_found_message(<the requested id>) }`. Only the message helper is shared; the resolve-and-classify branch is copied per handler, so the three copies can drift in the class they travel in or in whether they refuse at all, which is exactly the drift this crate has already chosen to design out for every other cross-operation verdict it has.

The crate's own pattern for precisely this situation is a refusal trait in the shared module plus one shared judge: `crate::state::CursorRefusal` + `judge_cursor` for the page-cursor verdict (three listings), `crate::state::ArnRefusal`/`ArnResourceRefusal` + `judge_connection_arn`/`connection_for_arn_mut` for the ARN-addressed verdict (three tag handlers), and `crate::state::direct_connect::AttachPointRefusal` + `judge_attach_point` for the attach-point verdict (two writers). Each of those traits' own documentation states the reason: the rule deciding WHICH refusal a verdict calls for and what it says lives once, and a handler supplies only which variant of its own channel it travels in. The environment verdict is the one remaining cross-operation verdict that does not go through such a seam.

The fix is the crate's existing shape — an `EnvironmentRefusal`-style trait plus one `environment_in_region_or_refuse::<E>(region, id) -> Result<Environment, E>` in `state.rs`, with the three handlers supplying only their own channel's variant. Every response, status, error code and message stays byte-identical, so the existing cross-operation test that pins the three answers as one answer stays green unchanged.

## Evidence

Copy 1 — services/service-com-amazonaws-interconnect-interconnect/src/ops/get_environment.rs:151-158:
```
        let Some(environment) = crate::state::environment_in_region(&region, &input.id) else {
            return Err(GetEnvironmentErrorModeled::InterconnectClientException(
                InterconnectClientException {
                    message: crate::state::environment_not_found_message(&input.id),
                },
            )
            .into());
        };
```

Copy 2 — src/ops/list_attach_points.rs:204-212:
```
        let Some(environment) = crate::state::environment_in_region(&region, &input.environment_id)
        else {
            return Err(ListAttachPointsErrorModeled::InterconnectClientException(
                InterconnectClientException {
                    message: crate::state::environment_not_found_message(&input.environment_id),
                },
            )
            .into());
        };
```

Copy 3 — src/ops/create_connection.rs:288-296 (inside `judge_against_store`):
```
    let Some(environment) = crate::state::environment_in_region(region, &input.environment_id)
    else {
        return Err(CreateConnectionErrorModeled::InterconnectClientException(
            InterconnectClientException {
                message: crate::state::environment_not_found_message(&input.environment_id),
            },
        )
        .into());
    };
```

The seam the crate already uses for every other cross-operation verdict, with its stated reason — src/state.rs:2243-2257:
```
/// The refusal a PAGE-CURSOR verdict can call for, in the error channel of the listing
/// that asked for it.
///
/// Every operation of this service carries its own modelled error enum, so the rule
/// deciding WHICH refusal a verdict about a `nextToken` calls for and what it says lives
/// once — in [`judge_cursor`] — and a paginated operation supplies only this: which
/// variant of its own channel that refusal travels in. A listing therefore cannot
/// classify or word this refusal differently from its neighbour.
pub trait CursorRefusal: Sized {
```
and the same shape for the ARN verdict — src/state.rs:1462-1470:
```
/// The further refusal an operation that ACTS ON the resource its `arn` names needs, and
/// a reporting one does not: see [`connection_for_arn_mut`].
pub trait ArnResourceRefusal: ArnRefusal {
    /// The `arn` addresses the scope the request is for and resolves to no resource of
    /// it, which is this service's own client fault rather than a malformed request: the
    /// modelled `InterconnectClientException` (HTTP 400), worded by
    /// [`arn_resource_absent_message`].
    fn arn_resource_absent(message: String) -> Self;
}
```
and for the attach-point verdict — src/state/direct_connect.rs:192-217:
```
/// The refusals an attach-point judgement can call for, in the error channel of the
/// operation that asked for it.
///
/// Every operation of this service carries its own modelled error enum, so the rules
/// deciding WHICH refusal a verdict calls for and what it says live once ... and an
/// operation that resolves an attach point supplies only this: which variant of its own
/// channel each refusal travels in. A handler therefore cannot classify or word one of
/// these refusals differently from its neighbour.
pub trait AttachPointRefusal: Sized {
```

The handlers themselves assert the three copies are ONE rule — src/ops/list_attach_points.rs:198-200: "An unresolvable environment is this service's own client fault ... so it is the 400 `InterconnectClientException` every environment-resolving operation gives"; src/ops/list_connections.rs:201-204: "the `environmentId` of every operation whose SUBJECT is an Environment (`GetEnvironment`, `ListAttachPoints`, `CreateConnection`), each of which refuses an id the called Region's catalog does not hold"; src/state.rs:1184-1187: "An unresolvable environment is a CLIENT fault for this service ... so it is reported as `InterconnectClientException`".

Test coverage the fix must leave unchanged — tests/all/list_connections_environment_filter.rs:226-244 and 366-382 drive all three operations through one assertion:
```
/// Assert `(status, body)` is the refusal an operation whose SUBJECT is an Environment
/// gives an id the called Region's catalog does not hold.
fn assert_environment_refused(case: &str, environment_id: &str, status: u16, body: &Value) {
```
```
                ("GetEnvironment", json!({ "id": id })),
                ("ListAttachPoints", json!({ "environmentId": id })),
                ...
                    "CreateConnection",
                ...
                let (status, body) = emulator.ic_in(region, operation, request).await;
                assert_environment_refused(&case, &id, status, &body);
```
Also covered by tests/all/region_scoped_environment_catalog.rs:91 (GetEnvironment), tests/all/refusals_and_gateway_attach_points.rs:50 (ListAttachPoints), tests/all/client_refusals_carry_the_envelope.rs:254 and :408, and tests/all/request_faults_precede_resource_verdicts.rs:686-694 and :994-1010 (CreateConnection).

## Steps to Reproduce

1. Open src/ops/get_environment.rs:151-158, src/ops/list_attach_points.rs:204-212 and src/ops/create_connection.rs:288-296 side by side: each spells the same branch — `crate::state::environment_in_region(...)` is `None` => `InterconnectClientException { message: crate::state::environment_not_found_message(<requested id>) }`.
2. Compare with src/state.rs:2243-2285 (`CursorRefusal` + `judge_cursor`), src/state.rs:1444-1518 (`ArnRefusal`/`ArnResourceRefusal` + `judge_connection_arn`/`connection_for_arn_mut`) and src/state/direct_connect.rs:192-271 (`AttachPointRefusal` + `judge_attach_point`): every other verdict this crate answers from more than one handler is decided once in the shared module, with the handler supplying only its own channel's variant.
3. Change the class or the wording in ONE of the three handler copies (e.g. make ListAttachPoints answer `ResourceNotFoundException`) and note that nothing in the shared module prevents it — only tests/all/list_connections_environment_filter.rs:366-382 would catch the drift, which is the drift the shared seams exist to make impossible.
4. Verify the fix is behaviour-preserving: route all three copies through one `environment_in_region_or_refuse::<E>(region, id)` plus a per-operation refusal-variant impl; the status (400), the error code (`InterconnectClientException`) and the message (`environment_not_found_message`) are unchanged, so every test listed in the evidence passes untouched.
