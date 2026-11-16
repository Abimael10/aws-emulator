Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 55eb763e8158a713d28edd00c224c291b78f1e19

# Fold the duplicated ClientToken replay/conflict rule in the two creates into one place in state.rs

_Filed: 2026-11-16T21:36:05Z_
_Operation: CreateChallenge | Class: maintainability_

> _Note: this issue was identified against the code base at commit `55eb763e8158a713d28edd00c224c291b78f1e19`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `@idempotencyToken` rule — "look the (Region, ClientToken) record up; if the resource it created is still answerable, refuse a token reused for a DIFFERENT request with ConflictException, otherwise answer with that resource; and on a fresh create file a new IdempotencyRecord under token_key(region, token)" — is implemented twice, once in `create_challenge.rs` and once in `create_connector.rs`, as two hand-written copies of the same branch structure.

This crate otherwise keeps a rule it applies in more than one handler in exactly ONE place in `state.rs`, and says so in that module's own docs: `tag_write_refusal` is "The ONE place this service decides and words that precondition, for every operation that changes a resource's tags ... so the sibling writes cannot answer one state with two sentences" (src/state.rs:572-576), `resources_not_active_message` is "The one place this service words that refusal, for every operation that has such a precondition — `CreateConnector` over the `CertificateAuthorityArn` it links, and `CreateChallenge` over the connector it mints a password under — so the two cannot state one precondition in two sentences" (src/state.rs:809-812), and `page_of` / `decode_cursor` are the single spelling of pagination for both listings. The token-replay rule is the one multi-handler rule left as two copies, so the two can drift: the shared half (fingerprint comparison, the "answerable ⇒ answer, else fall through" decision, and the `IdempotencyRecord` write) has no single owner, while the halves that genuinely differ (the liveness predicate and the error payload) are what a caller of a shared helper would pass in.

The fix is a `state.rs` helper (over `IdempotencyRecord` plus a liveness predicate) that both handlers call, leaving every response byte-identical: the ARNs answered, the 409 bodies and the fall-through behaviour are all already pinned by existing tests.

## Evidence

Copy 1 — src/ops/create_challenge.rs:227-252:
```
        if let Some(existing) = store.challenge_for_token(&region, input.client_token.as_deref()) {
            let answerable = store.challenge(&region, &existing.arn).filter(|challenge| {
                store
                    .connector(&region, &challenge.connector_arn)
                    .is_some_and(crate::state::StoredConnector::is_active)
            });
            if let Some(challenge) = answerable {
                // Same token, different request: the caller is not retrying the call this
                // token stands for.
                if existing.request != request {
                    return Err(CreateChallengeErrorModeled::ConflictException(ConflictException {
```
and its write, src/ops/create_challenge.rs:329-338:
```
        if let Some(token) = input.client_token {
            store.challenge_tokens.insert(
                crate::state::token_key(&region, &token),
                crate::state::IdempotencyRecord { arn, created_at: now, request },
```

Copy 2 — src/ops/create_connector.rs:327-346:
```
        if let Some(existing) = store.connector_for_token(&region, input.client_token.as_deref()) {
            if store
                .connector(&region, &existing.arn)
                .is_some_and(|connector| !connector.is_deleting())
            {
                // Same token, different request: the caller is not retrying the call this
                // token stands for, so answering with that connector would answer for a
                // request nobody made.
                if existing.request != request {
                    return Err(CreateConnectorErrorModeled::ConflictException(ConflictException {
```
and its write, src/ops/create_connector.rs:469-478:
```
        if let Some(token) = input.client_token {
            store.connector_tokens.insert(
                crate::state::token_key(&region, &token),
                crate::state::IdempotencyRecord { arn: arn.clone(), created_at: now, request },
```

The two copies already cross-reference each other instead of sharing code — src/ops/create_challenge.rs:225-226: "(The sibling create decides the same situation the same way — see `create_connector.rs`.)"

The centralisation convention they depart from, src/state.rs:572-576 (`tag_write_refusal`): "The ONE place this service decides and words that precondition, for every operation that changes a resource's tags (`TagResource` over the entries it merges, and `UntagResource` over the keys it drops), so the sibling writes cannot answer one state with two sentences."

Existing test coverage that must stay unchanged by the fold:
- tests/all/connector_lifecycle.rs:585 `client_token_reused_with_different_inputs_conflicts` — exercises BOTH copies' conflict path: the connector rows assert `(409, "ConflictException")` at lines 638-644 and the CreateChallenge half asserts the same at lines 674-681, plus the identical-retry answer at 647-652 and 682-692.
- tests/all/connector_lifecycle.rs:477 `client_token_makes_create_idempotent`.
- tests/all/challenge_client_token.rs:157 `a_client_token_answers_only_while_its_challenge_can_still_be_created` — the four `Between` rows that pin when the token stops binding.
- tests/all/connector_deletion.rs:456 `a_client_token_does_not_answer_with_a_connector_under_teardown`.
- tests/all/connector_private_ca_reachability.rs:477 `a_repeated_client_token_answers_the_connector_it_already_created`.
- tests/all/region_isolation.rs:657 `one_client_token_creates_one_connector_per_region` (the `token_key` half).

## Steps to Reproduce

1. Read src/ops/create_connector.rs:327-351 and src/ops/create_challenge.rs:227-252 side by side: the same four steps (token lookup, liveness test on the created resource, `existing.request != request` ⇒ ConflictException, else answer with the stored ARN) are written out twice.
2. Read src/ops/create_connector.rs:469-478 and src/ops/create_challenge.rs:329-338: the `IdempotencyRecord` write under `token_key(region, token)` is likewise written out twice.
3. Compare with src/state.rs:572-576, src/state.rs:809-812 and src/state.rs:1718 (`page_of`), where every other rule shared by two handlers of this crate lives in exactly one function that both call.
4. Confirm the behaviour is fully pinned before touching it: `cargo test -p service-com-amazonaws-pcaconnectorscep-pcaconnectorscep` runs client_token_reused_with_different_inputs_conflicts, client_token_makes_create_idempotent, a_client_token_answers_only_while_its_challenge_can_still_be_created, a_client_token_does_not_answer_with_a_connector_under_teardown and one_client_token_creates_one_connector_per_region.
5. Fold the shared half into one `state.rs` helper taking the token table, the fingerprint and a liveness predicate; both handlers keep only their own liveness predicate and their own error payload. Every response and every test above must be unchanged.
