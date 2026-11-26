Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: verifiedpermissions.delete-policy-by-name-removes-policy verifiedpermissions.strict-mode-schema-less-rejection-same-for-any-scope
Issue-Shape-Id: com.amazonaws.verifiedpermissions#VerifiedPermissions
Issue-Key: verifiedpermissions.delete-policy-by-name-removes-policy|expect_error#11|AssertionError|raise AssertionError(f"expected ({code!r}, {message!r}) but the call succeeded: {result!r}")
Issue-Key: verifiedpermissions.strict-mode-schema-less-rejection-same-for-any-scope|run#9|AssertionError|assert refusal.get("fieldList") == expected_field_list, (
Issue-Commit: e001304b013b7ad6eb10330af0cea4f05bddc43e

# VerifiedPermissions: a name/-prefixed policyId that names a policy whose own name carries the name/ prefix is never resolved, so DeletePolicy silently no-ops; and the STRICT-store-has-no-schema refusal blames /policyStoreId with emulator prose instead of /definition with AWS's sentence

> _Note: this issue was identified against the code base at commit `e001304b013b7ad6eb10330af0cea4f05bddc43e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeletePolicy

## Description
TWO distinct root causes in service-com-amazonaws-verifiedpermissions-verifiedpermissions. Both must be closed before this issue is submitted.

Cause 1 of 2: a `policyId` reference of the documented `name/<policyName>` form does not resolve when the policy's own stored name itself begins with `name/`, so DeletePolicy takes its idempotent-no-op branch and the policy survives a delete that answered 200.
AWS: Smithy (com.amazonaws.verifiedpermissions) DeletePolicyInput$policyId is @required, `PolicyId` len 1..200, pattern `^[a-zA-Z0-9-/_]*$`, documented "Specifies the ID of the policy that you want to delete. You can use the policy name in place of the policy ID. When using a name, prefix it with `name/`". `PolicyName` is len 0..150 with the same pattern, which ADMITS `/`, so `name/<something>` is itself a legal policy name. Recorded behavior: a policy created with `name` = `name/<n>` and then deleted with `policyId` = `name/<n>` IS removed — the subsequent GetPolicy on the minted policyId is refused 404 `ResourceNotFoundException` / `Resource not found` (with `resourceId` = the minted policy id and `resourceType` = `POLICY`, the members ResourceNotFoundException models as @required), and ListPolicies on the store returns no policies. Hypothesis: AWS matches the reference against the stored name without consuming the prefix as a bare separator (i.e. the full reference `name/<n>` also matches a policy named `name/<n>`), so such a policy is reachable by the value the caller used to name it.
Emulator: DeletePolicy answers an empty 200, but nothing is deleted. GetPolicy on the minted id still answers 200 with the full policy (policyType STATIC, the stored definition, effect Permit, name `name/<n>`) and ListPolicies still lists it. The reference resolver strips `name/` exactly ONCE and compares only the remainder to each policy's name (`PolicyStore::resolve_policy_id`), so for a policy named `name/<n>` the lookup looks for `<n>`, finds nothing, and the handler takes the documented "policy that doesn't exist" branch and returns success without writing.
Why: a client that deletes by the same reference it created the policy with is told the delete succeeded (empty 200, no error to key on) while the policy stays live and keeps being evaluated by IsAuthorized. That is silent authorization-state leakage: a revoked permit still permits, and no client-visible signal distinguishes it from a real delete. The same resolver backs every `policyId` member, so GetPolicy/UpdatePolicy by that reference answer 404/no-op for a policy that exists.
Fix: the policy-reference resolution in the crate's state (the `policyId` resolver all `policyId` members share) must resolve the reference AWS resolves, so the name/-prefixed form reaches a policy whose stored name carries that prefix; DeletePolicy's handler keeps its idempotent-success branch for a reference that genuinely names nothing.
Traces: verifiedpermissions.delete-policy-by-name-removes-policy.

Cause 2 of 2: the refusal a STRICT policy store holding no schema raises for a Cedar write names the wrong request member in `ValidationException$fieldList` and words it in emulator prose rather than the service's sentence.
AWS: Smithy ValidationException (400) carries `message` plus `fieldList: ValidationExceptionFieldList` of `ValidationExceptionField{path (required) "the path to the specific element that Verified Permissions found to be not valid", message (required) "describes the policy validation error"}`; CreatePolicy models ValidationException and takes `definition` (@required) and `policyStoreId` (@required). Recorded behavior for a store created with `validationSettings.mode = STRICT` and no schema: CreatePolicy with a static statement is refused 400 `ValidationException` with top-level message `Invalid input` and exactly one field, `path` = `/definition` and `message` = `No schema present - automatically failing all validation.` — fixed text, no value from the request or the store interpolated into it — and this is the SAME field whether or not the statement constrains a principal type (`permit(principal, action, resource);` and `permit(principal == Workspace::Member::"m1", action, resource);` produce identical Error and fieldList). PutSchema then lets a scoped statement through, and a store created in OFF mode accepts the unconstrained statement.
Emulator: the 400, the code `ValidationException` and the message `Invalid input` are right, but fieldList is `[{'path': '/policyStoreId', 'message': 'its validation mode is STRICT and it contains no schema, so there is nothing to validate a policy against'}]` — it blames the store-id member instead of `/definition`, and the message is the emulator's own prose (built in the shared store helper that decides what a write validates against, with the validated-content noun interpolated) rather than AWS's fixed sentence.
Why: `fieldList[].path` is the machine-readable part of a ValidationException; consoles, IaC providers and client-side validators use it to attribute the failure to a request field. Here it tells a client the policy store id is invalid when the policy definition is what AWS rejects, and a client matching the documented reason text sees a sentence AWS never emits. The same helper backs UpdatePolicy, CreatePolicyTemplate and UpdatePolicyTemplate, so all four writers answer the wrong field for this condition.
Fix: the shared "what does this store validate against" step in the crate's state (the branch that errors for a STRICT store with no schema) must raise the refusal against `/definition` with AWS's fixed sentence, for every Cedar write handler that calls it. Distinct from the open issue `20261116T004300Z-verifiedpermissions-createpolicy-error-response--r2.md`, which reports the ORDER in which CreatePolicy applies this check relative to parsing the statement and does not change the path or the sentence this refusal carries; also distinct from the fixed issues `20261022T174803Z-verifiedpermissions-control-plane-template-linked-createpolicy-updatepolicy-and.md` and `20261023T144912Z-verifiedpermissions-validationexception-contents-diverge-createidentitysource-uses-the-generic.md`, which made fieldList present and then complete for schema-validation diagnostics — this is the no-schema refusal's own path and text.
Traces: verifiedpermissions.strict-mode-schema-less-rejection-same-for-any-scope.

## Evidence
Cause 1 — check verifiedpermissions.delete-policy-by-name-removes-policy.
Sequence: CreatePolicyStore(validationSettings={'mode':'OFF'}) -> CreatePolicy(policyStoreId, name='name/<unique>', definition={'static':{'statement':'permit(principal, action, resource);'}}) -> GetPolicy(policyId=<minted id>) [positive control, passed] -> DeletePolicy(policyStoreId, policyId='name/<unique>') [answered 200] -> GetPolicy(policyStoreId, policyId=<minted id>).
Expected (AWS): the GetPolicy after the delete is refused ('ResourceNotFoundException', 'Resource not found') with resourceId = the minted policy id and resourceType = 'POLICY'; ListPolicies then returns [].
Actual (emulator): the GetPolicy SUCCEEDS with HTTP 200 and the full policy — AssertionError: "expected ('ResourceNotFoundException', 'Resource not found') but the call succeeded: {'policyStoreId': 'PS…', 'policyId': 'SP…', 'policyType': 'STATIC', 'definition': {'static': {'statement': 'permit(principal, action, resource);'}}, 'effect': 'Permit', 'name': 'name/sim2real-policy…', 'ResponseMetadata': {'HTTPStatusCode': 200, …}}" (ids/names/timestamps are per-run values, not requirements). The DeletePolicy itself raised nothing, so the no-op branch was taken.
Resolver that misses: services/service-com-amazonaws-verifiedpermissions-verifiedpermissions/src/state.rs:1398-1413 (`PolicyStore::resolve_policy_id` — `reference.strip_prefix("name/")` then compares only the remainder to `policy.name`); the handler branch it drives: src/ops/delete_policy.rs:145-158 (`let Some(policy_id) = store.resolve_policy_id(&input.policy_id) else { … return Ok(DeletePolicyOutput {}) }`).

Cause 2 — check verifiedpermissions.strict-mode-schema-less-rejection-same-for-any-scope.
Sequence: CreatePolicyStore(validationSettings={'mode':'STRICT'}) -> CreatePolicy(definition={'static':{'statement':'permit(principal, action, resource);'}}) and CreatePolicy(definition={'static':{'statement':'permit(principal == Workspace::Member::"m1", action, resource);'}}).
Matched on both calls: HTTP 400, Error.Code 'ValidationException', Error.Message 'Invalid input'.
Expected fieldList (AWS, both calls, identical): [{'path': '/definition', 'message': 'No schema present - automatically failing all validation.'}]
Actual fieldList (emulator, both calls): [{'path': '/policyStoreId', 'message': 'its validation mode is STRICT and it contains no schema, so there is nothing to validate a policy against'}]
AssertionError: "the unconstrained statement's fieldList was [{'path': '/policyStoreId', 'message': 'its validation mode is STRICT and it contains no schema, so there is nothing to validate a policy against'}], expected [{'path': '/definition', 'message': 'No schema present - automatically failing all validation.'}]".
Fixed text vs variable parts: AWS's sentence is wholly fixed — nothing in it comes from the request or the store — and the path is the constant JSON pointer '/definition'. The emulator's sentence interpolates the validated-content noun ('a policy' / 'a policy template') from the call site, which AWS does not do.
Source of the emulator's path and prose: src/state.rs ~1341-1351 (the `schema == None` branch of `PolicyStore::schema_to_validate_against`, built with `crate::ops::paths::POLICY_STORE_ID`), reached from src/ops/create_policy.rs:327-333 and from update_policy.rs / create_policy_template.rs / update_policy_template.rs.

## Steps to Reproduce
Cause 1:
1. CreatePolicyStore(validationSettings={'mode':'OFF'}).
2. CreatePolicy on that store with name='name/my-policy' and definition={'static':{'statement':'permit(principal, action, resource);'}}; keep the minted policyId.
3. GetPolicy(policyStoreId, policyId=<minted id>) — succeeds (control).
4. DeletePolicy(policyStoreId, policyId='name/my-policy') — answers 200.
5. GetPolicy(policyStoreId, policyId=<minted id>): expect 404 ResourceNotFoundException / 'Resource not found' with resourceId=<minted id>, resourceType='POLICY'; observe HTTP 200 with the policy still present. ListPolicies(policyStoreId) still lists it instead of returning [].
Replay check verifiedpermissions.delete-policy-by-name-removes-policy.

Cause 2:
1. CreatePolicyStore(validationSettings={'mode':'STRICT'}) and put NO schema in it.
2. CreatePolicy(policyStoreId, definition={'static':{'statement':'permit(principal, action, resource);'}}) and again with definition={'static':{'statement':'permit(principal == Workspace::Member::"m1", action, resource);'}}.
3. Inspect each 400 ValidationException: expect fieldList == [{'path': '/definition', 'message': 'No schema present - automatically failing all validation.'}] on both; observe path '/policyStoreId' with the emulator's STRICT/no-schema prose.
4. Confirm the follow-on expectations the check also makes: ListPolicies on the STRICT store is empty, PutSchema then lets a fully scoped statement through, and an OFF-mode store accepts the unconstrained statement.
Replay check verifiedpermissions.strict-mode-schema-less-rejection-same-for-any-scope.
