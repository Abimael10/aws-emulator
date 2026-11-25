Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: a70e125e8cb6958cfaca0f5c78511f414bfd42b9

# CreateIdentitySource refuses the request payload before the clientToken replay, inverting the order its sibling creates state and implement

_Filed: 2026-11-16T07:36:47Z_
_Operation: CreateIdentitySource | Class: error-response_

> _Note: this issue was identified against the code base at commit `a70e125e8cb6958cfaca0f5c78511f414bfd42b9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Of the four `@idempotencyToken` creates in this crate, `CreatePolicy` and `CreatePolicyTemplate` state and implement one order: resolve the referenced policy store, then settle the `clientToken` replay, and only THEN read and refuse the request's own payload. `CreateIdentitySource` does the opposite: it resolves and refuses the `configuration` member (and then `principalEntityType`) at src/ops/create_identity_source.rs:217 and :241, and only reaches the idempotency ledger afterwards at :266-267.

Both refusals it runs first are caller-facing 400s: `persisted_identity_source_configuration` goes through `crate::state::resolve_user_pool_arn`, which refuses a `cognitoUserPoolConfiguration.userPoolArn` naming a user pool that does not exist, and `persisted_principal_entity_type` refuses an OIDC write that leaves no `principalEntityType` to resolve.

So two documented outcomes of the token are unreachable whenever the payload no longer resolves:

* a replay of the SAME `clientToken` with the SAME parameters, made after the Cognito user pool the configuration named was deleted, answers 400 `ValidationException` instead of the idempotent success — the output of the identity source the first call created, which the handler's own replay branch at :279-285 is written to return;
* a replay of the same `clientToken` with DIFFERENT parameters whose configuration does not resolve answers 400 `ValidationException` instead of the `ConflictException` the handler's comment at :249-252 says the model documents for that case ("the same token with different parameters is the `ConflictException` the model documents").

This crate already treats payload-before-replay as the defect in this family: the open issue 20261115T224852Z-verifiedpermissions-createpolicystore-error-response.md records the same inversion for `CreatePolicyStore`'s `tags` check.

## Evidence

The rule, stated in this crate twice:

src/ops/create_policy.rs:314-325 — "// The SIZE BOUND on everything below: the cheap length check, before any\n// Cedar parse or schema validation, so no caller-sized statement reaches\n// the parser or the validator. ... It reads the request's PAYLOAD, so it\n// runs here: after the store the request references is resolved, and\n// after the `clientToken` replay, exactly where\n// `CreatePolicyTemplate` parses its statement."

src/ops/create_policy_template.rs:255-261 — "// The statement is Cedar TEMPLATE text, so the real Cedar template parser\n// decides whether it is a template at all. One it refuses is refused here,\n// BEFORE the name-conflict and quota checks: a malformed request is a 400\n// whatever else the store already holds. (The `clientToken` replay above\n// comes first, as it does in `CreatePolicy`: a token reused with different\n// parameters is a conflict however well-formed this statement is.)\ncrate::state::cedar::parse_policy_template(&input.statement).map_err(invalid)?;"

The handler that breaks it:

src/ops/create_identity_source.rs:215-222 — "// The provider reference is resolved before any state is CHANGED: a\n// request this service cannot resolve must change nothing.\nlet configuration = crate::ops::persisted_identity_source_configuration(\n    &*self.engine,\n    &input.configuration,\n    &region,\n)\n.map_err(invalid)?;"

src/ops/create_identity_source.rs:241-247 — "let principal_entity_type = crate::ops::persisted_principal_entity_type(\n    input.principal_entity_type.as_deref(), ... )\n.map_err(invalid)?;"

src/ops/create_identity_source.rs:249-252, 266-267 — "// `@idempotencyToken` replay: the same token answers with the identity\n// source the first call created, and the same token with different\n// parameters is the `ConflictException` the model documents." ... "if let Some(token) = input.client_token.as_deref() {\n    if let Some(record) = state.idempotency_replay(\"CreateIdentitySource\", token) {"

That the configuration refusal is a caller-facing 400 is pinned by the crate's own fixed issues 20261029T093530Z-verifiedpermissions-createidentitysource-validation.md and 20261030T015239Z-verifiedpermissions-createidentitysource-refuses-a-nonexistent-cognito-user-pool-with.md, and exercised by tests/all/identity_source_user_pool_existence.rs and tests/all/identity_source_pool_refusal_stages.rs. Replay behaviour is covered by tests/all/idempotency_parameter_escaping.rs and tests/all/identity_sources.rs; neither pins the relative order of the payload refusal and the replay.

## Steps to Reproduce

1. Create a Cognito user pool in the cognito-idp emulator and a policy store PS1 in Verified Permissions.
2. Call CreateIdentitySource with policyStoreId=PS1, clientToken="tok-1" and configuration.cognitoUserPoolConfiguration.userPoolArn naming that pool. Observe 200 and an identitySourceId (IS1).
3. Delete the Cognito user pool (or, equivalently, repeat the call with clientToken="tok-1" and a userPoolArn naming a pool that was never created).
4. Replay CreateIdentitySource with the identical body (policyStoreId=PS1, clientToken="tok-1", same configuration).
5. Observe 400 ValidationException from src/ops/create_identity_source.rs:217 — the ledger at :267 is never consulted — instead of the replay answering with IS1's stored output, or ConflictException when the replayed parameters differ.
6. Compare with CreatePolicyTemplate: replay a used clientToken with an unparseable `statement` and observe the ConflictException from its replay branch, which runs before the statement is parsed (src/ops/create_policy_template.rs:226-261).
