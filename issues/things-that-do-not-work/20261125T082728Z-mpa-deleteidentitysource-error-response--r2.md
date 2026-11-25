Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 9fdcc53790e48fd7e5c79662ae0d3b70846062d1

# The in-use ConflictException is the one refusal in the crate that interpolates a request value without the shared message bound

_Filed: 2026-11-25T08:27:28Z_
_Operation: DeleteIdentitySource | Class: error-response_

> _Note: this issue was identified against the code base at commit `9fdcc53790e48fd7e5c79662ae0d3b70846062d1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state` states ONE rule for every refusal this service builds by interpolating a value the request carried: it must go through `crate::state::bounded_message`, because every modelled `Message` member is `@length(max: 1000)` while the ARN-valued members a refusal quotes back are longer than that on their own. Ten refusal sites in this crate obey that rule (both creates, `UpdateApprovalTeam`, `CancelSession`, `StartApprovalTeamBaseline`, `DeleteInactiveApprovalTeamVersion`, `GetResourcePolicy`, `ListSessions`, plus the shared helpers in `state.rs`). `DeleteIdentitySource`'s IN-USE conflict is the only one that does not: it builds its `ConflictException$Message` with a bare `format!` that interpolates `input.identity_source_arn` — a member the model bounds at `@length(max: 1000)`, i.e. the WHOLE length the message may have — and the ARN of the team that uses it (`ApprovalTeamArn`, `@length(20..2048)`), plus ~110 characters of prose.

The message does not overrun its modelled bound today, but only by accident of which values can reach that branch: the branch is reached only for an ARN the store already holds a source under, and the stored keys and team ARNs are ones this service minted and therefore short. The bound the crate says it enforces by construction at this site is in fact enforced by nothing, so the invariant is one store-key change (a longer minted id, a restored record, a second producer of identity-source records) away from an answer outside the modelled shape. The crate's own bound test asserts the rule for a table of operations and never exercises this refusal, so nothing pins it either.

## Evidence

The rule the crate states for every refusal that quotes a request value — `src/state.rs:259-262`:
```rust
//! Whatever the sentence, a refusal is a modelled response shape: every `Message` this
//! service declares is `@length(max: 1000)` while the members a refusal quotes back run
//! to 2048 characters on their own, so every message built by interpolating a request
//! value is held to that bound by [`bounded_message`].
```
and `src/state.rs:282-288`:
```rust
/// Longest `Message` this service emits on a modelled error, from the
/// `@length(max: 1000)` every error's `Message` member declares — `ValidationException`,
/// `ConflictException`, ... It is the bound
/// [`bounded_message`] holds every refusal to.
pub const MAX_EXCEPTION_MESSAGE: usize = 1000;
```
and `src/state.rs:4399-4402`:
```rust
/// Every `Message` member of every error this service declares is `@length(max: 1000)`,
/// so this is the one bound, and every refusal built by interpolating a value the
/// REQUEST carried goes through here: a refusal is a modelled response shape, and a
/// request within its own constraints can never make this service answer outside them.
```

The site that skips it — `src/ops/delete_identity_source.rs:274-284`:
```rust
        if let Some(team_arn) = state.team_arn_using_identity_source(&input.identity_source_arn) {
            let message = format!(
                "Identity source '{}' is used by approval team '{team_arn}', so it cannot be \
                 deleted. Delete the approval teams that use it, and try again.",
                input.identity_source_arn
            );
            return Err(
                DeleteIdentitySourceErrorModeled::ConflictException(ConflictException { message })
                    .into(),
            );
        }
```

The sibling refusals of the same class, all holding to the helper — `src/ops/cancel_session.rs:216-223` (`message: crate::state::bounded_message(format!(... input.session_arn ...))`), `src/ops/update_approval_team.rs:264-270` and `:287-294` (both quote `input.arn`), `src/ops/create_approval_team.rs:290-296` (quotes `input.name`), `src/ops/start_approval_team_baseline.rs:201-211`, `:225-235`, `:237-245`, `:278-288` (quote `input.arn` and `ApproverIds` entries), `src/ops/delete_inactive_approval_team_version.rs:256-271` (quotes `input.arn` and `input.version_id`), `src/ops/get_resource_policy.rs:236-246` (quotes `input.policy_type`), `src/ops/list_sessions.rs:689-694` (quotes `Filter$Value`).

The model side of the bound — `smithy_query(query="shape",shape="ConflictException")`: `"Message": {"target": "String", "required": true, "length": {"max": 1000}}`; `DeleteIdentitySourceRequest$IdentitySourceArn` is `{"length": {"max": 1000}}` and `ApprovalTeamArn` is `{"length": {"min": 20, "max": 2048}}`.

Test coverage: `tests/all/refusal_message_bound.rs:3-22` states the rule for the whole service ("every message built by interpolating a value the request carried is held to the member's bound") and its table (`fn cases()`, `:258-330`; test `every_refusal_that_quotes_a_request_value_keeps_its_modelled_message_bound`, `:349`) spans `CreateApprovalTeam`, `UpdateApprovalTeam` and `ListSessions` only — the `DeleteIdentitySource` in-use conflict appears in no row, and `tests/all/identity_source_removal_in_use.rs` / `tests/all/identity_source_deletion.rs` assert the 409 and its wording without asserting the bound.

## Steps to Reproduce

1. Read `src/state.rs:259-262` and `:4399-4402`: the crate states that every refusal message built by interpolating a value the request carried goes through `bounded_message`, because each modelled `Message` is `@length(max: 1000)`.
2. Read `src/ops/delete_identity_source.rs:274-284`: the in-use `ConflictException` is built with a bare `format!` that interpolates `input.identity_source_arn` (modelled `@length(max: 1000)`) and a stored `ApprovalTeamArn` (modelled `@length(20..2048)`) plus ~110 characters of prose, with no `bounded_message` call.
3. Compare with every other refusal in the crate that quotes a request value (e.g. `src/ops/cancel_session.rs:216-223`, `src/ops/update_approval_team.rs:287-294`, `src/ops/start_approval_team_baseline.rs:278-288`): each wraps its `format!` in `crate::state::bounded_message`.
4. Grep the crate for `bounded_message`: `src/ops/delete_identity_source.rs` is the only handler that raises a value-quoting refusal and never calls it.
5. Check `tests/all/refusal_message_bound.rs:258-330`: no row exercises this refusal, so no test pins the bound for it.
