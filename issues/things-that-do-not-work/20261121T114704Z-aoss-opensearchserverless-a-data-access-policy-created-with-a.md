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
Issue-Commit: 1332dc451a9c2b4446e90cf355e9a9abe7c79725

# opensearchserverless: a data access policy created with a replayed clientToken can be neither read nor deleted — HTTP 400

> _Note: this issue was identified against the code base at commit `1332dc451a9c2b4446e90cf355e9a9abe7c79725`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAccessPolicy

## Description
2 causes, both in the aoss data-access-policy lifecycle exercised by the idempotent-clientToken fixture: once the `CreateAccessPolicy` calls of that fixture have run (the create and its clientToken replay both answer as recorded), the policy the fixture created can neither be READ back nor DELETED — both calls answer HTTP 400 instead of the recorded 200. Both must be closed before this issue is submitted.

Cause 1 of 2 — GetAccessPolicy answers 400 for a policy that exists (fixture opensearchserverless-create-access-policy-idempotent-clienttoken, step task#4).
AWS: Smithy: GetAccessPolicy(type: AccessPolicyType required, one of `data`; name: PolicyName required, @length 3..32, @pattern ^[a-z][a-z0-9-]+$) -> 200 with `accessPolicyDetail` (AccessPolicyDetail: type, name, policyVersion, description, policy, createdDate, lastModifiedDate). Its modeled errors are exactly InternalServerException (500), ResourceNotFoundException (@httpError 404, "Thrown when accessing or deleting a resource that does not exist") and ValidationException (@httpError 400, "Thrown when the HTTP request contains invalid input or is missing required input"). So for a well-formed read of a `data` policy whose name satisfies PolicyName there are only two AWS answers: 200 with the stored detail when the (type, name) pair is stored, and 404 ResourceNotFoundException when it is not. A 400 is reserved for a request that is itself invalid, which this one is not.
Emulator: the replayed step gets HTTP 400 and no `accessPolicyDetail` member at all, where the recording has 200 and the full detail for the policy the fixture created (type `data`, the fixture's own policy name, the policy document it submitted, a policyVersion and the two timestamps). The 400 is neither of the two answers the model permits for this request: it is not a 200 with the detail, and it is not the 404 a missing policy is reported with.
Why: a client that creates a data access policy and then reads it back — the standard create-then-confirm flow, and the flow every AOSS data-plane setup depends on, since the collection's data access is only usable once the policy it created is in force — is told its own valid request was invalid. A 400 ValidationException is not retryable and not a not-found, so the client cannot distinguish "my request was malformed" from "the policy is not there"; it aborts provisioning instead of either fixing the request or recreating the policy.
Fix: the GetAccessPolicy read path in the aoss service crate — either its pre-handler input validation (the only place in that path that can produce a 400: the boundary constraint/enum refusal rendered as ValidationException) or the CreateAccessPolicy state write that must leave the created policy addressable by its `(type, name)` pair after a clientToken replay, so the read resolves it. Whichever of the two it is, the operation must answer 200 with the stored detail when the pair is stored and ResourceNotFoundException at HTTP 404 (message `Policy with name <name> and type <type> is not found` — fixed text with the request's own name and type substituted) when it is not; 400 must be reserved for an input that actually violates the model.
Traces: opensearchserverless-create-access-policy-idempotent-clienttoken#task#4 (status: expected 200, got 400; $.accessPolicyDetail missing). Code to look at: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/ops/get_access_policy.rs lines 138-205 (handler: only 400 it can raise is the AccessPolicyType enum refusal at lines 163-180; its not-found exit at lines 183-197 is a 404), and src/ops/create_access_policy.rs lines 243-322 (the clientToken replay and the `(type, name)` insert).

Cause 2 of 2 — DeleteAccessPolicy answers 400 for the same policy (fixture opensearchserverless-create-access-policy-idempotent-clienttoken, step teardown#5).
AWS: Smithy: DeleteAccessPolicy(type required, name required, clientToken @idempotencyToken) is @idempotent, declares NO output members, and answers 200 with an empty body when the policy is deleted. Its modeled errors are ConflictException (409), InternalServerException (500), ResourceNotFoundException (404) and ValidationException (400) — so, as with the read, a well-formed delete of a `data` policy is either a 200 or, when the pair is not stored, a 404 ResourceNotFoundException.
Emulator: the teardown delete answers HTTP 400 where the recording has 200. The policy is therefore also undeletable, so the pair stays occupied for the rest of the session and a subsequent create of the same name would be answered as a conflict rather than succeeding.
Why: teardown never converges. A client (or a test harness, or CloudFormation-style cleanup) that deletes the policy it created is told the delete request was invalid, so it cannot clean up, cannot re-create the name, and has no modeled signal to retry on — a 400 is terminal.
Fix: the DeleteAccessPolicy write path in the aoss service crate, the same two candidate components as Cause 1 (its pre-handler input validation, and the stored `(type, name)` keying the removal resolves). Contract: 200 with an empty body when the pair is removed, 404 ResourceNotFoundException with the `Policy with name <name> and type <type> is not found` wording when it is not stored, and 400 only for an input the model refuses.
Traces: opensearchserverless-create-access-policy-idempotent-clienttoken#teardown#5 (status: expected 200, got 400). Code to look at: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/ops/delete_access_policy.rs lines 140-208 (its only 400 is the enum refusal at lines 167-184; the not-found exit at lines 187-201 is a 404) and src/state.rs lines 3883-3889 (`remove_access_policy`).

Note: no open issue covers these steps. The open aoss issues are about the published collectionEndpoint host, the bedrock-agent knowledge-base storage configuration, aoss control-plane IAM authorization, and service-quotas default quota rows; none of them concerns the access-policy read/delete status, and none carries these fixture steps as its replay key. The IAM gate every aoss operation is served behind answers 403 AccessDeniedException (src/service/authorization.rs line 129), so it is not the source of these 400s.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 150 steps, 145 passed, 3 mismatched).

Cause 1 — fixture opensearchserverless-create-access-policy-idempotent-clienttoken, step task#4, operation GetAccessPolicy, verdict mismatch:
  expected: HTTP 200 with $.accessPolicyDetail = {type: "data", name: <the fixture's policy name>, description: <the description the create submitted>, policy: [the submitted statement list], policyVersion: <the version the create minted>, createdDate: <epoch millis>, lastModifiedDate: <epoch millis>}
  actual:   HTTP 400, $.accessPolicyDetail missing (no detail member in the body)
  divergence_key: service=aoss|GetAccessPolicy|status=200->400|keys=$.accessPolicyDetail,status

Cause 2 — fixture opensearchserverless-create-access-policy-idempotent-clienttoken, step teardown#5, operation DeleteAccessPolicy, verdict mismatch:
  expected: HTTP 200 with an empty body (the operation models no output members)
  actual:   HTTP 400
  divergence_key: service=aoss|DeleteAccessPolicy|status=200->400|keys=status

Model evidence (smithy_query on services/service-com-amazonaws-opensearchserverless-opensearchserverless/smithy-model/com-amazonaws-opensearchserverless-opensearchserverless.json): GetAccessPolicy errors = InternalServerException(500), ResourceNotFoundException(404), ValidationException(400); DeleteAccessPolicy errors = ConflictException(409), InternalServerException(500), ResourceNotFoundException(404), ValidationException(400); ResourceNotFoundException @httpError 404; ValidationException @httpError 400. Neither operation has any modeled 400 answer for a valid request, and an absent policy is a 404 — so HTTP 400 is unreachable for the request these steps send.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus: replay_wire_corpus() against trace set simparity-wire-aws-v1.
2. Cause 1: fixture opensearchserverless-create-access-policy-idempotent-clienttoken, step task#4 — the fixture creates a `data` access policy with a clientToken, re-sends the create with the same clientToken, then calls GetAccessPolicy with {type: "data", name: <that policy's name>}. Observe HTTP 400 with no accessPolicyDetail instead of the recorded HTTP 200 carrying the stored detail.
3. Cause 2: same fixture, step teardown#5 — DeleteAccessPolicy with {type: "data", name: <that policy's name>}. Observe HTTP 400 instead of the recorded HTTP 200 with an empty body.
4. Both steps are the whole reproduction: re-replay those two fixture steps after the fix and both must pass (200 + detail, and 200 + empty body).
