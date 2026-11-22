Issue-Source: trace-sweep-divergence
Issue-Trace: opensearchserverless-create-access-policy-idempotent-clienttoken
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearchserverless-create-access-policy-idempotent-clienttoken
Issue-Shape-Id: com.amazonaws.opensearchserverless#OpenSearchServerless
Issue-Key: aoss|GetAccessPolicy|2xx->4xx|$.accessPolicyDetail
Issue-Key: aoss|GetAccessPolicy|2xx->4xx|status
Issue-Key: aoss|DeleteAccessPolicy|2xx->4xx|status
Issue-Commit: 48fdd4b276fb760420919974f00f6d6e0750e9b1

# opensearchserverless: access policy created with a clientToken cannot be read or deleted — GetAccessPolicy and DeleteAccessPolicy answer 400

> _Note: this issue was identified against the code base at commit `48fdd4b276fb760420919974f00f6d6e0750e9b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAccessPolicy

## Description
This issue bundles 2 net-new causes, both on the `aoss` (OpenSearch Serverless) data access policy surface and both observed in the same replayed fixture. A data access policy that the fixture's earlier CreateAccessPolicy steps created successfully (those steps PASSED, including the repeated-clientToken idempotency probe) is afterwards neither readable nor deletable: every subsequent call that addresses it by its `(type, name)` pair is refused with HTTP 400 instead of succeeding. The shared fault is that the `(type, name)` addressing of a stored access policy is refused at the request-validation boundary rather than resolved against the store, so the resource becomes unreachable the moment the creating call returns. A fixer must close BOTH causes below before submitting.

--- Cause 1 of 2: GetAccessPolicy refuses a stored data access policy with HTTP 400 instead of returning it ---

AWS: `GetAccessPolicy` is `@readonly` and returns HTTP 200 with an `accessPolicyDetail` (`AccessPolicyDetail`) member carrying `type`, `name`, `policyVersion`, `description`, `policy` (the JSON policy document without whitespace), `createdDate` and `lastModifiedDate` for the policy stored under the requested `(type, name)` pair. Smithy: `GetAccessPolicy` input `type` (`AccessPolicyType`, the single value `data`) required, `name` (`PolicyName`, @length min 3 max 32, @pattern `^[a-z][a-z0-9-]+$`) required; output `accessPolicyDetail` -> `AccessPolicyDetail`; modeled errors `ValidationException` (@httpError 400), `ResourceNotFoundException` (@httpError 404), `InternalServerException` (@httpError 500). The two 4xx outcomes are therefore disjoint and exhaustive for a client: a WELL-FORMED request for a policy that exists is a 200, and a well-formed request for a policy that does NOT exist is a 404 `ResourceNotFoundException` ("Thrown when accessing or deleting a resource that does not exist") — a 400 is reserved for `ValidationException`, "thrown when the HTTP request contains invalid input or is missing required input". The replayed request is well formed on every modeled constraint: `type` is the enum's only value `data`, and the policy name is a lowercase/digit/hyphen token of 30 characters that satisfies both the length bound and the pattern.

Emulator: the replay step answers HTTP 400 and the response body carries no `accessPolicyDetail` at all. Replay detail: `status: expected 200, got 400` and `$.accessPolicyDetail: missing`. Because `ValidationException` is the ONLY 400 these operations model, the emulator is reporting the caller's input as invalid or missing for a request whose every member satisfies the model's constraints; and it is not even reporting the alternative legitimate outcome, since an absent policy would have to be a 404 `ResourceNotFoundException` whose message names the policy and its type. Note that the operation's own handler (`services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/ops/get_access_policy.rs`) has only two refusal paths, an `InternalServerException` when the store is unreachable and a `ValidationException` when `type` is outside `ACCESS_POLICY_TYPE_VALUES`, with the not-found path returning `ResourceNotFoundException` — so a 400 on `type = data` means the refusal is decided before the store is consulted. Hypothesis: the `(type, name)` pair of the request is rejected by the boundary validator or by the enum guard on `type` rather than being used as the store key, so the lookup never runs.

Why: a client cannot read back a data access policy it just created. Every normal control-plane flow — create-then-verify, read-modify-write through `UpdateAccessPolicy` (which needs the current `policyVersion` as its optimistic-concurrency precondition), and any CloudFormation/Terraform-style reconciliation that reads a resource to decide whether it drifted — breaks. Worse, the 400 class misleads the client: an AWS SDK treats `ValidationException` as a non-retryable caller bug and surfaces it as bad input, so a caller is told its own request is malformed instead of learning that the policy is missing (404) or being handed the policy (200). Retry and wait-for-existence logic keyed on `ResourceNotFoundException` never fires.

Fix: `GetAccessPolicy`'s handler and its input validation — the `(type, name)` resolution must reach the access-policy store and answer the stored record with 200, reserving 400 `ValidationException` for input that actually violates a modeled constraint and 404 `ResourceNotFoundException` for a pair no policy is stored under. The message of a correct refusal is a template, not a recorded string: its fixed text is AWS's wording for the error class, with the policy name and the policy type taken from the request, never from one recording.

Traces: `opensearchserverless-create-access-policy-idempotent-clienttoken#task#4` (expected 200 + `accessPolicyDetail`, got 400 with no detail).

--- Cause 2 of 2: DeleteAccessPolicy refuses the same stored policy with HTTP 400 instead of deleting it ---

AWS: `DeleteAccessPolicy` is `@idempotent` and returns HTTP 200 with an EMPTY body on success — the model declares no output members, so the observable effect is the policy's absence from the store afterwards. Smithy: `DeleteAccessPolicy` input `type` (`AccessPolicyType`) required, `name` (`PolicyName`, @length 3..32, @pattern `^[a-z][a-z0-9-]+$`) required, `clientToken` (`ClientToken`, @idempotencyToken, @length 1..512) optional; output `DeleteAccessPolicyResponse` with no members; modeled errors `ConflictException` (@httpError 409), `ResourceNotFoundException` (@httpError 404), `ValidationException` (@httpError 400), `InternalServerException` (@httpError 500). As with the read, a well-formed delete of an existing policy is a 200 and a well-formed delete of a nonexistent one is a 404 `ResourceNotFoundException`; 400 is only for input that violates a modeled constraint.

Emulator: the teardown step answers HTTP 400. Replay detail: `status: expected 200, got 400`. The request addresses the very policy the fixture's create steps stored, with the same well-formed `type = data` and the same name that satisfies `PolicyName`'s length and pattern, so the only modeled 400 — `ValidationException` — is being returned for valid input. The handler in `services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/ops/delete_access_policy.rs` likewise refuses only on an out-of-enum `type` before it reaches `remove_access_policy`, whose own miss path is a 404 `ResourceNotFoundException`.

Why: the resource leaks and cannot be cleaned up. A test or provisioning run that creates a data access policy can never remove it, so the account accumulates policies against the per-account access-policy quota until `CreateAccessPolicy` starts answering `ServiceQuotaExceededException`; and because a delete is modeled `@idempotent`, SDK and infrastructure-as-code teardown paths retry it and keep getting a non-retryable 400, so teardown never converges and the whole fixture/stack is left undeletable.

Fix: `DeleteAccessPolicy`'s handler and its input validation — resolve the `(type, name)` pair against the access-policy store, remove the record and answer an empty 200, reserving 400 `ValidationException` for genuinely invalid input and 404 `ResourceNotFoundException` for a pair that names no stored policy (with the name and type interpolated from the request).

Traces: `opensearchserverless-create-access-policy-idempotent-clienttoken#teardown#5` (expected 200 empty body, got 400).

Related open issues, none of which covers either cause: `20261121T114704Z-aoss-opensearchserverless-createvpcendpoint-words-its-missing-vpc-validationexception-unlike.md` (CreateVpcEndpoint ValidationException wording — a different operation, and it already owns its own replay key), `20261114T213110Z-...-aoss-control-plane-calls-are-never-authorized...` (authorization of aoss control-plane calls against the caller's IAM identity policy — an authorization gap, not a 400 on a well-formed read/delete), and `20261109T154357Z-opensearchserverless-the-published-collectionendpoint-...` (collection endpoint host).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 150 steps, 145 passed, 3 mismatched, 2 blocked, score 96.7%. Two net-new aoss mismatches, both in fixture `opensearchserverless-create-access-policy-idempotent-clienttoken` (the fixture's earlier CreateAccessPolicy / repeated-clientToken steps PASSED, so the policy was created and the idempotent replay was served correctly):

Cause 1 — step `task#4`, operation GetAccessPolicy, verdict mismatch:
  expected_status 200, actual_status 400
  detail: "status: expected 200, got 400\n$.accessPolicyDetail: missing (expected {'createdDate': '<shaped-not-asserted>', 'description': 'idempotent clientToken probe', 'lastModifiedDate': '<shaped-not-asserted>', 'name': '<policy-name>', 'policy': [{'Rules': [{'Resource': ['collection/*'], 'Permission': ['aoss:DescribeCollectionItems'], 'ResourceType': 'collection'}], 'Principal': ['arn:aws:iam::<account>:root']}], 'policyVersion': '{{policy_version}}', 'type': 'data'}, got None)"
  divergence_key: service=aoss|GetAccessPolicy|status=200->400|keys=$.accessPolicyDetail,status
  Expected: HTTP 200 whose body carries `accessPolicyDetail` with type `data`, the requested policy name, the policy document as stored, the minted `policyVersion`, and the create/last-modified timestamps. Actual: HTTP 400 with no `accessPolicyDetail`. Per the model the only 400 on this operation is `ValidationException` (@httpError 400) — `ResourceNotFoundException` is @httpError 404 — so the emulator reports invalid input for a request whose `type` is the enum's only value `data` and whose name satisfies `PolicyName` @length 3..32 and @pattern ^[a-z][a-z0-9-]+$.

Cause 2 — step `teardown#5`, operation DeleteAccessPolicy, verdict mismatch:
  expected_status 200, actual_status 400
  detail: "status: expected 200, got 400"
  divergence_key: service=aoss|DeleteAccessPolicy|status=200->400|keys=status
  Expected: HTTP 200 with an empty body (the model declares no output members) and the policy gone from the store. Actual: HTTP 400, policy not deleted. Again the only modeled 400 is `ValidationException`; a pair naming no stored policy would have to be 404 `ResourceNotFoundException`.

The only generated-name/per-run values above (the policy name suffix and the account id) are environment-specific and are NOT part of the requirement; the requirement is the status, the presence and shape of `accessPolicyDetail`, and the error class/message template when a refusal is correct.

Smithy evidence (services/service-com-amazonaws-opensearchserverless-opensearchserverless/smithy-model/com-amazonaws-opensearchserverless-opensearchserverless.json, OpenSearchServerless 2021-11-01, awsJson1_0, sigv4Name aoss):
  GetAccessPolicy: @readonly; input {type: AccessPolicyType required, name: PolicyName required len 3..32 pattern ^[a-z][a-z0-9-]+$}; output {accessPolicyDetail: AccessPolicyDetail}; errors ValidationException(400), ResourceNotFoundException(404), InternalServerException(500).
  DeleteAccessPolicy: @idempotent; input {type required, name required, clientToken @idempotencyToken len 1..512}; output {} (no members); errors ConflictException(409), ResourceNotFoundException(404), ValidationException(400), InternalServerException(500).
  AccessPolicyType: string enum, values [data].
  AccessPolicyDetail: {type, name, policyVersion (len 20..36), description, policy (document), createdDate (Long), lastModifiedDate (Long)}.
  CreateAccessPolicy: @idempotent, clientToken @idempotencyToken; output {accessPolicyDetail} — which is why a following read must report the same record.

Emulator code the two causes land in: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/ops/get_access_policy.rs (handle: only refusal paths are InternalServerException for an unreachable store and ValidationException when `type` is outside ACCESS_POLICY_TYPE_VALUES; the store miss returns ResourceNotFoundException) and services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/ops/delete_access_policy.rs (same shape, with the miss on state::remove_access_policy returning ResourceNotFoundException).

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this checkout (the aoss data listener answers; /__admin/health returned 200 during this replay).
2. Replay the wire fixture `opensearchserverless-create-access-policy-idempotent-clienttoken` from trace set `simparity-wire-aws-v1` end to end. Its CreateAccessPolicy steps (including the repeated-`clientToken` idempotency probe) pass, so a `data` access policy is stored under its `(type, name)` pair.
3. Cause 1 — step `task#4`: call GetAccessPolicy with `{"type": "data", "name": "<the policy name just created>"}` (awsJson1_0, X-Amz-Target OpenSearchServerless.GetAccessPolicy). Expected HTTP 200 with an `accessPolicyDetail` carrying that type/name, the stored policy document, the minted `policyVersion`, `description`, `createdDate` and `lastModifiedDate`. Observed HTTP 400 with no `accessPolicyDetail`.
4. Cause 2 — step `teardown#5`: call DeleteAccessPolicy with `{"type": "data", "name": "<the same policy name>"}`. Expected HTTP 200 with an empty body and the policy removed from the store. Observed HTTP 400 and the policy still present.
5. Both steps are well formed against the model (`type` is `AccessPolicyType`'s only value `data`; the name satisfies `PolicyName` @length 3..32 and @pattern ^[a-z][a-z0-9-]+$), so neither 400 can be a legitimate `ValidationException`; a genuinely absent policy would have to be 404 `ResourceNotFoundException`.
6. Re-replay both `opensearchserverless-create-access-policy-idempotent-clienttoken#task#4` and `opensearchserverless-create-access-policy-idempotent-clienttoken#teardown#5` to confirm a fix — closing only the read leaves the undeletable-policy cause live.
