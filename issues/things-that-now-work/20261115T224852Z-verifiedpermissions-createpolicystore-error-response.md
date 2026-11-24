Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: f53b60d37da65728f91690e0e656985cb7bded33

# CreatePolicyStore refuses an over-limit `tags` map before the clientToken replay, inverting the precedence its sibling creates state and implement

_Filed: 2026-11-15T22:48:52Z_
_Operation: CreatePolicyStore | Class: error-response_

> _Note: this issue was identified against the code base at commit `f53b60d37da65728f91690e0e656985cb7bded33`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every `@idempotencyToken` create in this crate states one precedence for the two refusals a request can draw: the `clientToken` replay is decided FIRST, and a refusal about the request's own payload only afterwards — so a token reused with different parameters is the modelled `ConflictException` however badly the rest of the request is formed. `CreatePolicy` and `CreatePolicyTemplate` both implement that order and both say so in a comment.

`CreatePolicyStore` does the opposite. Its own payload check — the per-resource tag-limit refusal on `input.tags` — runs at the very top of `handle`, before the state lock is even taken and therefore before `idempotency_replay`. The consequence: a caller that successfully created a store with `clientToken` T and then replays T with a `tags` map over the per-resource quota is answered 400 `ValidationException` naming `/tags`, where the sibling rule the crate states for the same pair of refusals answers 409 `ConflictException` naming the store the token already created.

The tag check's own comment justifies only its position relative to MINTING ("before anything is read or minted, so a request that cannot be served leaves no store behind"); it says nothing about why it must also outrank the replay — and the replay mints nothing either, so moving the check below it keeps that justification intact. Note this is NOT the situation in `CreateIdentitySource`, whose pre-replay work (`persisted_identity_source_configuration` / `persisted_principal_entity_type`) is structurally required because `configuration.fingerprint()` and `principal_entity_type.value` are both members of the `parameters` string the replay compares; the tag-count refusal is not needed to build `CreatePolicyStore`'s `parameters`.

## Evidence

The rule, stated by a sibling — services/service-com-amazonaws-verifiedpermissions-verifiedpermissions/src/ops/create_policy_template.rs:255-261:

```
        // The statement is Cedar TEMPLATE text, so the real Cedar template parser
        // decides whether it is a template at all. One it refuses is refused here,
        // BEFORE the name-conflict and quota checks: a malformed request is a 400
        // whatever else the store already holds. (The `clientToken` replay above
        // comes first, as it does in `CreatePolicy`: a token reused with different
        // parameters is a conflict however well-formed this statement is.)
```

and the same rule restated and implemented in services/service-com-amazonaws-verifiedpermissions-verifiedpermissions/src/ops/create_policy.rs:314-325 (the replay sits above it at lines 285-306):

```
        // The SIZE BOUND on everything below: ... It reads the request's PAYLOAD, so it
        // runs here: after the store the request references is resolved, and
        // after the `clientToken` replay, exactly where
        // `CreatePolicyTemplate` parses its statement.
        if let PolicyDefinition::Static(def) = &input.definition {
            crate::state::cedar::check_statement_size(&def.statement).map_err(invalid)?;
        }
```

The other side — services/service-com-amazonaws-verifiedpermissions-verifiedpermissions/src/ops/create_policy_store.rs:167-182, the FIRST statement of `handle`:

```
        // The request's `tags` map is the tag set the new store would start out
        // holding, so it is admitted only if a resource may hold it: ...
        // Refused before anything is read or minted, so a request that
        // cannot be served leaves no store behind — one that the tagging path
        // would then treat as permanently over the limit.
        if let Some(tags) = input.tags.as_ref() {
            if crate::ops::tags_over_the_per_resource_limit(tags.len()) {
                return Err(CreatePolicyStoreErrorModeled::ValidationException(
```

while the replay this is supposed to follow comes only at create_policy_store.rs:196-221:

```
        let parameters = canonical_parameters(&input);
        if let Some(token) = input.client_token.as_deref() {
            if let Some(record) = state.idempotency_replay("CreatePolicyStore", token) {
                if record.parameters != parameters {
                    return Err(CreatePolicyStoreErrorModeled::ConflictException(
```

Test coverage: tests/all/create_time_tag_limit.rs::a_refused_create_persists_nothing pins only the case where the token has NO prior record (over-limit + token -> 400, then the same token with an admissible map -> 200); both orders satisfy it, so moving the check below the replay leaves that test and every response it asserts unchanged. No test in the crate exercises a replay of an already-used token with an over-limit tag map.

## Steps to Reproduce

1. CreatePolicyStore with {"validationSettings":{"mode":"OFF"},"clientToken":"a1b2c3d4","tags":{"k":"v"}} — a store is created and the token recorded.
2. CreatePolicyStore again with the SAME "clientToken":"a1b2c3d4" and a `tags` map of 60 distinct keys (over crate::state::MAX_TAGS_PER_RESOURCE).
3. Observe 400 ValidationException with fieldList naming /tags.
4. Compare with the sibling rule: CreatePolicy/CreatePolicyTemplate with a reused clientToken and a payload their own checks would refuse answer 409 ConflictException naming the resource the token already created (create_policy.rs:285-306 runs before create_policy.rs:323-325; create_policy_template.rs:226-247 runs before create_policy_template.rs:261).
