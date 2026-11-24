Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore.get-memory-record-cross-tenant-read-denied
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore.get-memory-record-cross-tenant-read-denied|run#11|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: ba3dc5862a56424c18ec8ce124a357736c03e24c

# bedrock-agentcore: the GetMemoryRecord AccessDeniedException message is a generic "is not authorized to perform this operation" instead of AWS's action/resource-naming authorization sentence

> _Note: this issue was identified against the code base at commit `ba3dc5862a56424c18ec8ce124a357736c03e24c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMemoryRecord

## Description
One cause.

Cause 1 of 1: the data-plane authorization refusal for GetMemoryRecord carries a truncated, emulator-authored message. Failing check: bedrock-agentcore.get-memory-record-cross-tenant-read-denied.

AWS: GetMemoryRecord is modeled with AccessDeniedException at HTTP 403 (Smithy: GetMemoryRecord errors AccessDeniedException errorKind=client httpError=403; input members memoryId (@httpLabel, pattern allows a bare id or a memory ARN) and memoryRecordId (@httpLabel)). The model states the error shape and status but not the message text, so the message requirement here is recorded behavior: when an identity-based policy grants bedrock-agentcore:GetMemoryRecord only on one memory ARN and the call targets a different memory, AWS refuses with the standard IAM authorization sentence, whose fixed text is "User: ", " is not authorized to perform: ", " on resource: ", " because no identity-based policy allows the " and " action", and whose variable parts come from the request and the caller's credentials: the caller's assumed-role principal ARN (arn:aws:sts::<account>:assumed-role/<role-name>/<session-name>), the action namespace-qualified by the operation being called (bedrock-agentcore:GetMemoryRecord), and the ARN of the resource the request targeted (arn:aws:bedrock-agentcore:<region>:<account>:memory/<memoryId>). The action appears twice: once after "to perform:" and once before " action".

Emulator: the refusal is correctly classified AccessDeniedException, but its message stops after the principal: "User: <assumed-role principal ARN> is not authorized to perform this operation". Everything AWS says after the principal — the action, the "on resource: <target ARN>" clause and the "because no identity-based policy allows the <action> action" attribution — is missing, and the request's memoryId is never reflected into the message at all.

Why: a client cannot tell from this refusal which action or which resource was denied, so the per-resource nature of the grant is invisible. Tooling and tests that parse the standard IAM sentence to extract the action or the denied resource ARN (the usual way an operator diagnoses a scoped-down policy, and the way least-privilege test suites assert that a grant is enforced per memory) see a message that matches nothing, and a cross-tenant denial is indistinguishable from any other authorization failure. An emulator that denies the right call but cannot say what it denied also hides the opposite defect: a denial produced for the wrong reason reads identically.

Fix: the authorization-denial path that renders AccessDeniedException for the bedrock-agentcore data plane — the message construction used when the identity-based policy evaluation returns no matching Allow for GetMemoryRecord. It must compose AWS's sentence from the evaluated request: the caller principal ARN, the service-qualified action for the operation, and the resource ARN built from the request's memoryId (expanding a bare memory id to arn:aws:bedrock-agentcore:<region>:<account>:memory/<id>, and using the ARN as given when the input is already an ARN), followed by the "because no identity-based policy allows the <action> action" attribution. The status (403) and the error code (AccessDeniedException) are already correct and must not change.

Traces: bedrock-agentcore.get-memory-record-cross-tenant-read-denied — a role whose inline policy allows bedrock-agentcore:GetMemoryRecord on only one memory ARN calls GetMemoryRecord against a second memory id; the check asserts code AccessDeniedException (passes) and then the message (fails).

## Evidence
Check bedrock-agentcore.get-memory-record-cross-tenant-read-denied, [mismatch]:

AssertionError: expected message 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform: bedrock-agentcore:GetMemoryRecord on resource: arn:aws:bedrock-agentcore:<region>:<account>:memory/<other-memory-id> because no identity-based policy allows the bedrock-agentcore:GetMemoryRecord action', got 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform this operation'

(The run's concrete values were account 000000000000, role sim2real-behav-role-93b68e61a449, session sim2real-behav-session, region us-west-2, memory id tenantb-9dy9hhgzfc — per-run values that fill the template, not part of it.)

Expected (AWS): HTTP 403, code AccessDeniedException, message = "User: <principal ARN> is not authorized to perform: bedrock-agentcore:GetMemoryRecord on resource: <memory ARN> because no identity-based policy allows the bedrock-agentcore:GetMemoryRecord action".
Actual (emulator): HTTP 403, code AccessDeniedException, message = "User: <principal ARN> is not authorized to perform this operation" — no action, no resource, no attribution clause.

Smithy (services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json): GetMemoryRecord GET /memories/{memoryId}/memoryRecord/{memoryRecordId}; errors include AccessDeniedException (client, httpError 403).

## Steps to Reproduce
1. Create an IAM role with an inline policy whose only statement is Allow bedrock-agentcore:GetMemoryRecord on Resource arn:aws:bedrock-agentcore:<region>:<account>:memory/<memoryA>.
2. Assume that role (session name of your choice) and build a bedrock-agentcore client from the returned temporary credentials.
3. Call GetMemoryRecord with memoryId=<memoryB> (a different memory id) and any well-formed memoryRecordId (mem-<40 hex chars>).
4. Observe the refusal: the code is AccessDeniedException at 403, but the message is "User: <principal ARN> is not authorized to perform this operation" instead of AWS's "... is not authorized to perform: bedrock-agentcore:GetMemoryRecord on resource: arn:aws:bedrock-agentcore:<region>:<account>:memory/<memoryB> because no identity-based policy allows the bedrock-agentcore:GetMemoryRecord action".
