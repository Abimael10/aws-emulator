Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 0124150dcd3f028fd1f7b257c908b2802f0f11c2

# The "resolve the nextToken or refuse it" step is written out once per paginated handler, five times over

_Filed: 2026-11-19T06:29:01Z_
_Operation: ListEnvironmentConnectors | Class: maintainability_

> _Note: this issue was identified against the code base at commit `0124150dcd3f028fd1f7b257c908b2802f0f11c2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Each of the five `@paginated` handlers carries its own copy of the same step: map `input.next_token` through `decode_page_token`, discard WHICH `PageTokenError` came back, and lift `invalid_next_token()` into that operation's `ValidationException`. The rule itself is already single-homed in `crate::state` (`decode_page_token` decides what makes a token resumable, `invalid_next_token` words the one refusal, and the crate deliberately single-homed the sibling page bound in `page_size` "so no two paginated operations of this service can bound their pages differently"), but the branch that joins them is duplicated per handler, so the copies can drift: one of them could start distinguishing `Expired` from `Malformed`, or stop refusing, without the others changing.

The fix is one generic helper in `crate::state` — e.g. `fn page_cursor(key, scope, next_token) -> Result<Option<String>, ValidationException>` — that each handler lifts into its own error enum with a single `map_err`. Every response and every existing test stays byte-identical: the status, the message and the `fieldList` all keep coming from `invalid_next_token`.

## Evidence

Copy 1 — src/ops/list_environments.rs:199-210:
```
        let start_after = match input.next_token.as_deref() {
            None => None,
            Some(token) => Some(
                crate::state::decode_page_token(&page_token_key, &scope, token).map_err(
                    |_| {
                        ListEnvironmentsErrorModeled::ValidationException(
                            crate::state::invalid_next_token(),
                        )
                    },
                )?,
            ),
        };
```

Copy 2 — src/ops/list_environment_connectors.rs:200-211:
```
        let start_at = match input.next_token.as_deref() {
            None => None,
            Some(token) => Some(
                crate::state::decode_page_token(&page_token_key, &scope, token).map_err(
                    |_| {
                        ListEnvironmentConnectorsErrorModeled::ValidationException(
                            crate::state::invalid_next_token(),
                        )
                    },
                )?,
            ),
        };
```

The same block appears again at src/ops/list_environment_hosts.rs:194-205, src/ops/list_vm_entitlements.rs:211-222 and src/ops/list_environment_vlans.rs:191-206 (which adds its own cursor-to-`i32` mapping on top).

The rule the copies lean on is already single-homed: src/state.rs:3822-3847 (`decode_page_token`) and src/state.rs:3863-3882 (`invalid_next_token`, "Why a token could not be resumed ([`PageTokenError`]) decides nothing here: Amazon EVS answers [`INVALID_NEXT_TOKEN_MESSAGE`] for every token a listing cannot resume"), and src/state.rs:3690-3693 (`page_size`): "The rule lives beside the range it is stated over, so no two paginated operations of this service can bound their pages differently."

Tests that pin the behaviour and must stay unchanged: tests/all/page_token_binding.rs (a token of another listing/selection/store is refused) and tests/all/page_size_bound.rs (the page bound of all five listings).

## Steps to Reproduce

1. Read src/ops/list_environments.rs:199-210 and src/ops/list_environment_connectors.rs:200-211: the same five-line verdict-to-refusal branch, differing only in the error enum it names.
2. Repeat for src/ops/list_environment_hosts.rs:194-205, src/ops/list_vm_entitlements.rs:211-222 and src/ops/list_environment_vlans.rs:191-206 — five copies of one step.
3. Note that `crate::state` already owns both halves it joins (`decode_page_token`, `invalid_next_token`) and that the sibling page bound was single-homed in `page_size` for this exact reason.
4. Confirm with `cargo test -p service-com-amazonaws-evs-amazonelasticvmwareservice` that page_token_binding and page_size_bound pass before and after extracting the shared helper.
