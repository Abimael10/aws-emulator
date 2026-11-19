Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 3d40b4bdd1a74693919d044a2652faf1f3ecdc56

# The "a nextToken this listing did not issue is refused" verdict is mapped to a refusal three times, once per paginated handler, although the crate already shares such mappings through a refusal trait

_Filed: 2026-11-19T02:15:55Z_
_Operation: ListConnections | Class: maintainability_

> _Note: this issue was identified against the code base at commit `3d40b4bdd1a74693919d044a2652faf1f3ecdc56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate shares every other cross-operation verdict through a refusal trait, so the rule that decides WHICH modelled error a verdict travels in lives once and a handler supplies only the variant of its own channel. The page-cursor verdict is the one exception: `crate::state::decode_cursor` answering `None` is turned into a refusal by three byte-for-byte identical blocks, one inside each paginated handler (`ListConnections`, `ListEnvironments`, `ListAttachPoints`). The shared part is only the message builder (`cursor_not_issued_message`); the decode-or-refuse branch and the error CLASSIFICATION (`InterconnectValidationException`) are restated per handler, so the three copies can drift — a fourth listing, or an edit to one of the three, can classify or branch on the same fault differently from its neighbours, which is exactly what the crate's `ArnRefusal`/`AttachPointRefusal` docs say a handler must not be able to do.

The fix is the shape the crate already uses twice: a `CursorRefusal` trait with one method (`cursor_not_issued(message) -> Self`) plus a `judge_cursor::<E>(token_key, input.next_token.as_deref(), scope)` helper in `state.rs` that returns `Result<Option<String>, E>`; each handler then implements the one method and calls the helper. Every response body, status and message stays byte-identical, and no existing test changes.

## Evidence

Copy 1 — services/service-com-amazonaws-interconnect-interconnect/src/ops/list_connections.rs:252-265:
```
        let cursor = match input.next_token.as_deref() {
            Some(token) => {
                let Some(key) = crate::state::decode_cursor(&token_key, token, &scope) else {
                    return Err(ListConnectionsErrorModeled::InterconnectValidationException(
                        InterconnectValidationException {
                            message: crate::state::cursor_not_issued_message("ListConnections"),
                        },
                    )
                    .into());
                };
                Some(key)
            }
            None => None,
        };
```

Copy 2 — services/service-com-amazonaws-interconnect-interconnect/src/ops/list_environments.rs:222-235:
```
        let cursor = match input.next_token.as_deref() {
            Some(token) => {
                let Some(key) = crate::state::decode_cursor(&token_key, token, &scope) else {
                    return Err(ListEnvironmentsErrorModeled::InterconnectValidationException(
                        InterconnectValidationException {
                            message: crate::state::cursor_not_issued_message("ListEnvironments"),
                        },
                    )
                    .into());
                };
                Some(key)
            }
            None => None,
        };
```

Copy 3 — services/service-com-amazonaws-interconnect-interconnect/src/ops/list_attach_points.rs:207-220:
```
        let cursor = match input.next_token.as_deref() {
            Some(token) => {
                let Some(key) = crate::state::decode_cursor(&token_key, token, &scope) else {
                    return Err(ListAttachPointsErrorModeled::InterconnectValidationException(
                        InterconnectValidationException {
                            message: crate::state::cursor_not_issued_message("ListAttachPoints"),
                        },
                    )
                    .into());
                };
                Some(key)
            }
            None => None,
        };
```

The crate's own stated rule for the two verdicts it DOES share — services/service-com-amazonaws-interconnect-interconnect/src/state.rs:1397-1402 (`ArnRefusal`):
```
/// Every operation of this service carries its own modelled error enum, so the rules
/// deciding WHICH refusal a verdict about an `AmazonResourceName` calls for and what it
/// says live once — in [`judge_connection_arn`] for the ARN itself — and an operation
/// addressed by an ARN supplies only this: which variant of its own channel each refusal
/// travels in. A handler therefore cannot classify or word one of these refusals
/// differently from its neighbour.
```
and services/service-com-amazonaws-interconnect-interconnect/src/state/direct_connect.rs:195-201 (`AttachPointRefusal`):
```
/// Every operation of this service carries its own modelled error enum, so the rules
/// deciding WHICH refusal a verdict calls for and what it says live once — in
/// [`judge_attach_point`] for the gateway resource and in
/// [`crate::state::judge_attach_point_in_store`] for the member's own constraints and the
/// ARN's resource — and an operation that resolves an attach point supplies only this:
/// which variant of its own channel each refusal travels in.
```

The message half is already shared and says so — services/service-com-amazonaws-interconnect-interconnect/src/state.rs:2030-2031:
```
/// The ONE wording every paginated operation of this service refuses an unknown cursor
/// with ([`decode_cursor`] answering `None`), so the three listings cannot word the
/// same fault differently.
```
so only the branch and the classification are left duplicated.

Test coverage that pins the behaviour the refactor must not change: services/service-com-amazonaws-interconnect-interconnect/tests/all/page_cursors_are_only_what_the_listing_minted.rs:12-18 drives the rule "over all three paginated operations of this service: a `nextToken` is accepted only when it is one THAT listing minted ... and every other value is refused with HTTP 400 `InterconnectValidationException` and this service's one wording for a cursor it did not issue, never with a page"; also tests/all/attach_points.rs:198 ("a paged walk, at any page size, yields exactly the unpaged listing") and tests/all/connection_lifecycle.rs.

## Steps to Reproduce

1. Open services/service-com-amazonaws-interconnect-interconnect/src/ops/list_connections.rs and read lines 252-265: the decode-or-refuse branch plus the `InterconnectValidationException` classification.
2. Open src/ops/list_environments.rs lines 222-235 and src/ops/list_attach_points.rs lines 207-220: the same branch and classification, restated verbatim with only the operation name and the error enum changed.
3. Compare with src/state.rs:1397-1410 (`ArnRefusal`) and src/state/direct_connect.rs:195-217 (`AttachPointRefusal`), where the crate states that such a verdict's classification must live once so "a handler therefore cannot classify or word one of these refusals differently from its neighbour".
4. Note that only the MESSAGE is shared today (src/state.rs:2035 `cursor_not_issued_message`), so nothing stops one of the three listings from classifying an unissued cursor differently.
5. Run `cargo test -p service-com-amazonaws-interconnect-interconnect` before and after introducing a `CursorRefusal` trait + `judge_cursor` helper: tests/all/page_cursors_are_only_what_the_listing_minted.rs and every other test must pass unchanged.
