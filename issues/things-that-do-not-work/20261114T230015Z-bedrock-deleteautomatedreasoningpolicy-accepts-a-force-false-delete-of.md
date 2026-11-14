Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-automated-reasoning-policy-lifecycle
Issue-Step: teardown#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-automated-reasoning-policy-lifecycle
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|DeleteAutomatedReasoningPolicy|4xx->2xx|status
Issue-Commit: fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8

# bedrock DeleteAutomatedReasoningPolicy accepts a force=false delete of a policy that still has artifacts instead of refusing 400

> _Note: this issue was identified against the code base at commit `fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAutomatedReasoningPolicy

## Description
ONE cause: the DeleteAutomatedReasoningPolicy handler deletes unconditionally and never evaluates the artifact pre-condition that the `force` query parameter gates, so a delete AWS refuses with a 4xx client error is accepted as a success.

Cause 1 of 1 — DeleteAutomatedReasoningPolicy does not validate outstanding policy artifacts when force is false (fixture bedrock-automated-reasoning-policy-lifecycle, step teardown#9).

AWS: Smithy (com.amazonaws.bedrock#AmazonBedrockControlPlaneService) DeleteAutomatedReasoningPolicy -> http {DELETE, /automated-reasoning-policies/{policyArn}}, success code 202, @idempotent. Input: policyArn (@httpLabel, @required, AutomatedReasoningPolicyArn, @length 1..2048, @pattern ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:automated-reasoning-policy/[a-z0-9]{12}(:([1-9][0-9]{0,11}))?$) and force (@httpQuery "force", Boolean, @default false). The model documents the rule on that member verbatim: "Specifies whether to force delete the automated reasoning policy even if it has active resources. When false, Amazon Bedrock validates if all artifacts have been deleted (e.g. policy version, test case, test result) for a policy before deletion. When true, Amazon Bedrock will delete the policy and all its artifacts without validation. Default is false." The refusal is therefore modelled, not incidental: the operation's 400-class errors are ConflictException (httpError 400, "Error occurred because of a conflict while performing an operation."), ResourceInUseException (httpError 400, "Thrown when attempting to delete or modify a resource that is currently being used by other resources or operations. For example, trying to delete an Automated Reasoning policy that is referenced by an active guardrail.") and ValidationException (httpError 400); ResourceNotFoundException is 404. The recording for this step is a 400: the fixture creates a policy, updates it, cuts version 1 of it (CreateAutomatedReasoningPolicyVersion at task#6, which AWS answered 200) and then teardown deletes the base policy ARN with force left at its default false, and AWS refuses the delete because the policy's artifacts — the published version — still exist. For the error code and message this is recorded-status-only: the replay asserts the HTTP status for this step and does not expose the recorded body, so the only thing established here is the 400 status and the fact that the delete must NOT take effect; which of the three modelled 400 errors AWS returns (and its message text) is not asserted by this fixture, and ResourceInUseException is the error whose documentation describes exactly this condition. Hypothesis: AWS returns ValidationException or ResourceInUseException naming the undeleted artifacts; do not hard-code any message from this recording.

Emulator: answers HTTP 202 — the modelled SUCCESS code — and deletes the policy. No pre-condition is evaluated at all: the handler neither reads the `force` query parameter's effect nor enumerates the policy's artifacts (versions, test cases, test results) before removing it. Note the emulator reaches this step from a different state than the recording, because the version-creation step earlier in the same fixture (task#6) is itself refused 400 by this build — that divergence is already owned by the open issue 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md (cause 3). That makes the emulator's 202 explainable but not correct: the requirement is that DeleteAutomatedReasoningPolicy with force=false CHECK for outstanding artifacts and refuse 400 when any exist, and that check does not exist in this build, so the delete would be accepted even once versioning works.

Why: a client relying on AWS's behavior uses force=false precisely as a safety interlock — it deletes a policy only if nothing still depends on it, and handles the 400 by cleaning up versions/test cases first (or re-issuing with force=true deliberately). Against this emulator the interlock silently does not exist: the policy and its artifacts are destroyed on the first attempt, the caller sees 202 and never enters its cleanup/confirmation branch, and anything that still references the policy (an active guardrail, a pinned policy version) is left pointing at a resource that no longer exists. Because the wrong answer is the modelled success code, nothing in the client surfaces the difference. This is the same over-permissive-delete family as the open behavioral issue 20261029T172312Z-bedrock-control-plane-is-over-permissive-enforced-guardrail.md (cause 1, DeleteGuardrail accepted while an enforced-guardrail configuration still targets it); that issue does not cover this operation or this replay key.

Fix: DeleteAutomatedReasoningPolicy's handler plus the Automated Reasoning policy state it deletes from — read the `force` httpQuery flag, and when it is false enumerate the policy's dependent artifacts (policy versions, test cases, test results, and references from active guardrails) in state and refuse with the modelled 400 client error, leaving the policy and its artifacts intact; only when force is true delete the policy and all its artifacts without validation. The 202 success path stays as modelled for the cases that pass the check.

Traces: bedrock-automated-reasoning-policy-lifecycle#teardown#9 (expected 400, actual 202).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, bootstrap_signal false, 142/172 steps passed, score 82.6%). This cluster is reported with stub_path "" and owned_by [] — it is net-new and no open issue carries its replay key.

Cluster: divergence_key service=bedrock|DeleteAutomatedReasoningPolicy|status=400->202|keys=status, service bedrock, operation DeleteAutomatedReasoningPolicy, verdict mismatch, fixture_id bedrock-automated-reasoning-policy-lifecycle, step_key teardown#9, affected_count 1 (bedrock-automated-reasoning-policy-lifecycle#teardown#9).

Expected vs actual:
- status: expected 400 (the recorded AWS response — a client-error refusal from DeleteAutomatedReasoningPolicy's modelled 400 set: ConflictException / ResourceInUseException / ValidationException), got 202 (the operation's modelled SUCCESS code, i.e. the delete was performed).
- No body paths are reported diverging for this step: the replay asserts the status for teardown#9 and does not expose the recorded error body, so the error code and message are NOT established by this fixture and must not be copied from a recording. Fixed requirement: HTTP 400 and no deletion. Variable parts of whatever message AWS words for this error class (the policy ARN/id, the names or counts of the undeleted artifacts) come from the request and the emulator's state.

Surrounding state in the same fixture (why the recording is a 400): task#4 UpdateAutomatedReasoningPolicy, task#6 CreateAutomatedReasoningPolicyVersion (recorded 200, producing version "1" and the versioned '<policy-arn>:1' ARN), then teardown#9 deletes the base policy ARN with no force query parameter, i.e. force=false. In this build task#6 is refused 400 (already owned by 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md), so the emulator holds no version at teardown time; that explains its 202 but does not satisfy the requirement, since the handler performs no artifact check in any case.

Model evidence (smithy_query on services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json):
- operation DeleteAutomatedReasoningPolicy: http {method DELETE, uri /automated-reasoning-policies/{policyArn}, code 202}, idempotent; input policyArn @httpLabel @required, force @httpQuery "force" Boolean @default false; errors AccessDeniedException 403, ConflictException 400, InternalServerException 500, ResourceInUseException 400, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400.
- doc DeleteAutomatedReasoningPolicyRequest$force: "...When false, Amazon Bedrock validates if all artifacts have been deleted (e.g. policy version, test case, test result) for a policy before deletion. When true, Amazon Bedrock will delete the policy and all its artifacts without validation. Default is false."
- shape ResourceInUseException: errorKind client, httpError 400, member message (NonBlankString) — "Thrown when attempting to delete or modify a resource that is currently being used by other resources or operations. For example, trying to delete an Automated Reasoning policy that is referenced by an active guardrail."
- shape ConflictException: errorKind client, httpError 400, member message (NonBlankString).

## Steps to Reproduce
1. Clone this repo and boot the emulator built from the checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Replay fixture bedrock-automated-reasoning-policy-lifecycle end to end; the diverging step is teardown#9.
4. Reproduce the step directly: CreateAutomatedReasoningPolicy with the fixture's definition, UpdateAutomatedReasoningPolicy, then CreateAutomatedReasoningPolicyVersion (POST /automated-reasoning-policies/{policyArn}/versions) so the policy has at least one artifact — version "1". (While 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md is open this call is refused 400 on the emulator; to exercise THIS defect in isolation, create any policy artifact the build does support, or inspect the handler to confirm no artifact check is performed.)
5. Issue DELETE /automated-reasoning-policies/{policyArn} with NO force query parameter (force defaults to false). Observed: HTTP 202 and the policy is gone. Expected: HTTP 400 from DeleteAutomatedReasoningPolicy's modelled client-error set, with the policy and its artifacts still present and readable via GetAutomatedReasoningPolicy.
6. Control: re-issue the identical delete with ?force=true. Expected (and must keep working): HTTP 202, policy and all artifacts removed.
7. Confirm the fix by re-replaying bedrock-automated-reasoning-policy-lifecycle#teardown#9; it must report the recorded 400.
